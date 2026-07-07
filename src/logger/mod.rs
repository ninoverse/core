use chrono::Local;
use std::sync::OnceLock;
use std::sync::mpsc::{self, Sender};
use std::thread;
use std::fmt;

use crate::debug;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[allow(dead_code)]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

impl fmt::Display for LogLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let level_str = match self {
            LogLevel::Debug => "DEBUG",
            LogLevel::Info  => "INFO",
            LogLevel::Warn  => "WARN",
            LogLevel::Error => "ERROR",
        };
        write!(f, "{}", level_str)
    }
}

struct LogMessage {
    level: LogLevel,
    message: String,
    decorator: Vec<String>,
}

static LOG_SENDER: OnceLock<Sender<LogMessage>> = OnceLock::new();

pub fn init() {
    let (log_sender, log_receiver) = mpsc::channel::<LogMessage>();

    if LOG_SENDER.set(log_sender).is_err() {
        debug!(["LOGGER"], "Warning: Logger was already initialized.");
        return;
    }

    thread::spawn(move || {
        while let Ok(log_item) = log_receiver.recv() {
            match log_item.level {
                LogLevel::Debug => print_stdout(log_item),
                LogLevel::Info => print_stdout(log_item),
                LogLevel::Warn => print_stdout(log_item),
                LogLevel::Error => print_stderr(log_item),
            }
        }
    });
}

fn print_stdout(log_item: LogMessage) {
    let now = Local::now().format("%Y-%m-%d %H:%M:%S");

    println!(
        "{} | {} | {} | {}",
        now,
        log_item.level.to_string(),
        log_item.decorator.join(" - "),
        log_item.message
    )
}

fn print_stderr(log_item: LogMessage) {
    let now = Local::now().format("%Y-%m-%d %H:%M:%S");

    eprintln!(
        "{} | {} | {} | {}",
        now,
        log_item.level.to_string(),
        log_item.decorator.join(" - "),
        log_item.message
    )
}

pub fn log(message: &str, level: LogLevel, decorator: Option<Vec<String>>) {
    if let Some(sender) = LOG_SENDER.get() {
        let _ = sender.send(LogMessage {
            level,
            message: message.to_string(),
            decorator: decorator.unwrap_or_default(),
        });
    } else {
        eprintln!("Logger not initialized! Missed log: {}", message);
    }
}

#[macro_export]
macro_rules! debug {
    // 1. Decorator array + plain string
    ([$($dec:expr),*], $msg:expr) => {
        $crate::logger::log($msg, $crate::logger::LogLevel::Debug, Some(vec![$($dec.to_string()),*]))
    };
    // 2. Decorator array + formatted string
    ([$($dec:expr),*], $msg:expr, $($arg:tt)*) => {
        $crate::logger::log(&format!($msg, $($arg)*), $crate::logger::LogLevel::Debug, Some(vec![$($dec.to_string()),*]))
    };
    // 3. No decorator + plain string
    ($msg:expr) => {
        $crate::logger::log($msg, $crate::logger::LogLevel::Debug, None)
    };
    // 4. No decorator + formatted string
    ($msg:expr, $($arg:tt)*) => {
        $crate::logger::log(&format!($msg, $($arg)*), $crate::logger::LogLevel::Debug, None)
    };
}

#[macro_export]
macro_rules! info {
    ([$($dec:expr),*], $msg:expr) => {
        $crate::logger::log($msg, $crate::logger::LogLevel::Info, Some(vec![$($dec.to_string()),*]))
    };
    ([$($dec:expr),*], $msg:expr, $($arg:tt)*) => {
        $crate::logger::log(&format!($msg, $($arg)*), $crate::logger::LogLevel::Info, Some(vec![$($dec.to_string()),*]))
    };
    ($msg:expr) => {
        $crate::logger::log($msg, $crate::logger::LogLevel::Info, None)
    };
    ($msg:expr, $($arg:tt)*) => {
        $crate::logger::log(&format!($msg, $($arg)*), $crate::logger::LogLevel::Info, None)
    };
}

#[macro_export]
macro_rules! warn {
    ([$($dec:expr),*], $msg:expr) => {
        $crate::logger::log($msg, $crate::logger::LogLevel::Warn, Some(vec![$($dec.to_string()),*]))
    };
    ([$($dec:expr),*], $msg:expr, $($arg:tt)*) => {
        $crate::logger::log(&format!($msg, $($arg)*), $crate::logger::LogLevel::Warn, Some(vec![$($dec.to_string()),*]))
    };
    ($msg:expr) => {
        $crate::logger::log($msg, $crate::logger::LogLevel::Warn, None)
    };
    ($msg:expr, $($arg:tt)*) => {
        $crate::logger::log(&format!($msg, $($arg)*), $crate::logger::LogLevel::Warn, None)
    };
}

#[macro_export]
macro_rules! error {
    ([$($dec:expr),*], $msg:expr) => {
        $crate::logger::log($msg, $crate::logger::LogLevel::Error, Some(vec![$($dec.to_string()),*]))
    };
    ([$($dec:expr),*], $msg:expr, $($arg:tt)*) => {
        $crate::logger::log(&format!($msg, $($arg)*), $crate::logger::LogLevel::Error, Some(vec![$($dec.to_string()),*]))
    };
    ($msg:expr) => {
        $crate::logger::log($msg, $crate::logger::LogLevel::Error, None)
    };
    ($msg:expr, $($arg:tt)*) => {
        $crate::logger::log(&format!($msg, $($arg)*), $crate::logger::LogLevel::Error, None)
    };
}