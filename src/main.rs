use std::fmt::{Debug, Display};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail};
use async_stream::stream;
use contracts::{ensures, requires};
use futures::StreamExt;
use futures::stream::BoxStream;
use libnotify::{Notification, Urgency};
use tokio::fs::read;
use tokio::time::sleep;

const APP_NAME: &'static str = "battery-notifier";
const BATTERY_CHARGING: &'static str = "/sys/class/power_supply/BAT0/status";
const BATTERY_LEVEL_NOW: &'static str = "/sys/class/power_supply/BAT0/energy_now";
const BATTERY_LEVEL_FULL: &'static str = "/sys/class/power_supply/BAT0/energy_full";
const CRITICAL_BATTERY_LEVEL: BatteryLevel = BatteryLevel(6);
const LOW_BATTERY_LEVEL: BatteryLevel = BatteryLevel(15);

async fn battery_charging() -> Result<bool, anyhow::Error> {
    let raw_charging_level = String::from_utf8(read(BATTERY_CHARGING).await?)?;
    match raw_charging_level.trim() {
        "Charging" => Ok(true),
        "Unknown" | "Discharging" | "Not charging" | "Full" => Ok(false),
        _ => Err(anyhow!("Invalid charging status")),
    }
}

async fn battery_energy_full() -> Result<f32, anyhow::Error> {
    let raw_battery_level = String::from_utf8(read(BATTERY_LEVEL_FULL).await?)?;
    // println!("Raw battery level: {raw_battery_level}");
    let res: u32 = raw_battery_level.trim().parse()?;

    Ok(res as f32)
}

async fn battery_energy_now() -> Result<f32, anyhow::Error> {
    let raw_battery_level = String::from_utf8(read(BATTERY_LEVEL_NOW).await?)?;
    // println!("Raw battery level: {raw_battery_level}");
    let res: u32 = raw_battery_level.trim().parse()?;

    Ok(res as f32)
}

struct NotificationService;

impl NotificationService {
    fn new(app_name: &str) -> Result<NotificationService, anyhow::Error> {
        if let Err(e) = libnotify::init(app_name) {
            bail!("Failed to initialize libnotify with err = {e}");
        }
        Ok(NotificationService)
    }

    fn notify_critical_battery(&self, level: BatteryLevel) -> Result<(), anyhow::Error> {
        let notification = Notification::new(
            "Battery Critical!",
            format!("Battery critical at {}", level).as_str(),
            "battery-caution",
        );
        notification.set_urgency(Urgency::Critical);
        notification.set_timeout(i32::MAX);
        notification.show()?;
        Ok(())
    }

    fn notify_low_battery(&self, level: BatteryLevel) -> Result<(), anyhow::Error> {
        let notification = Notification::new(
            "Battery Low!",
            format!("Battery low at {}", level).as_str(),
            "battery-low",
        );
        notification.show()?;
        notification.set_urgency(Urgency::Critical);
        notification.set_timeout(i32::MAX);
        Ok(())
    }
}

impl Drop for NotificationService {
    fn drop(&mut self) {
        libnotify::uninit();
    }
}

#[derive(Clone, PartialOrd, Ord, Eq, PartialEq)]
struct BatteryLevel(u8);

impl BatteryLevel {
    #[requires(percent.clone().try_into().is_ok_and(|v| v <= 100))]
    #[ensures(ret.0 <= 100)]
    fn new<P: TryInto<u8, Error = E> + Clone, E: Debug>(percent: P) -> Self {
        let level = percent.try_into().expect("could not convert to u8");
        BatteryLevel(level)
    }

    #[ensures(ret <= 100)]
    #[ensures(ret == self.0)]
    fn level(&self) -> u8 {
        self.0
    }
}

impl Display for BatteryLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}%", self.0)
    }
}

fn calc_battery_level(current: f32, total: f32) -> BatteryLevel {
    let level: i32 = (((current / total) * 100.0).round() as i32).min(100);
    BatteryLevel::new(level)
}

fn battery_level_stream() -> BoxStream<'static, BatteryLevel> {
    Box::pin(stream! {
        let total = battery_energy_full().await
            .expect("Failed to get full battery level");

        loop {
            let current = battery_energy_now().await
                .expect("Failed to get current battery level");

            yield calc_battery_level(current, total);
        }
    })
}

enum NotificationState {
    NotifiedLow(Instant),
    NotifiedCritical(Instant),
    Charging,
    NeverNotified,
}

async fn battery_notifier() -> Result<(), anyhow::Error> {
    use NotificationState::*;

    let notification_service: NotificationService = NotificationService::new(APP_NAME)?;

    let mut battery_stream = battery_level_stream();
    let mut notification_state = NeverNotified;
    let crit_frequency = Duration::from_secs(60);
    let low_frequency = Duration::from_secs(5 * 60);

    while let Some(level) = battery_stream.next().await {
        println!("Current battery: {level}");
        let now = Instant::now();
        let battery_charging = battery_charging().await?;

        if battery_charging {
            notification_state = Charging
        } else if !battery_charging
            && level <= CRITICAL_BATTERY_LEVEL
            && !matches!(notification_state,
                 NotifiedCritical(t) if now.duration_since(t) < crit_frequency)
        {
            println!("Battery critical!");
            notification_service.notify_critical_battery(level)?;
            notification_state = NotifiedCritical(now)
        } else if !battery_charging
            && level <= LOW_BATTERY_LEVEL
            && !matches!(notification_state,
                NotifiedLow(t) if now.duration_since(t) < low_frequency)
            && !matches!(notification_state,
                NotifiedCritical(t) if now.duration_since(t) < low_frequency)
        {
            println!("Battery low!");
            notification_service.notify_low_battery(level)?;
            notification_state = NotifiedLow(now)
        }

        sleep(Duration::from_secs(60)).await;
    }

    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), anyhow::Error> {
    battery_notifier().await?;

    Ok(())
}
