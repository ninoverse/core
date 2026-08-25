use chrono::Local;
use std::fmt;
use std::sync::OnceLock;
use std::sync::mpsc::{self, Sender};
use std::thread;

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
            LogLevel::Info => "INFO",
            LogLevel::Warn => "WARN",
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
        warn!(["LOGGER"], "Warning: Logger was already initialized.");
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
        log_item.level,
        log_item.decorator.join(" - "),
        log_item.message
    )
}

fn print_stderr(log_item: LogMessage) {
    let now = Local::now().format("%Y-%m-%d %H:%M:%S");

    eprintln!(
        "{} | {} | {} | {}",
        now,
        log_item.level,
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
    ([$($dec:expr),*], $msg:expr) => {
        $crate::log($msg, $crate::LogLevel::Debug, Some(vec![$($dec.to_string()),*]))
    };
    ([$($dec:expr),*], $msg:expr, $($arg:tt)*) => {
        $crate::log(&format!($msg, $($arg)*), $crate::LogLevel::Debug, Some(vec![$($dec.to_string()),*]))
    };
    ($msg:expr) => {
        $crate::log($msg, $crate::LogLevel::Debug, None)
    };
    ($msg:expr, $($arg:tt)*) => {
        $crate::log(&format!($msg, $($arg)*), $crate::LogLevel::Debug, None)
    };
}

#[macro_export]
macro_rules! info {
    ([$($dec:expr),*], $msg:expr) => {
        $crate::log($msg, $crate::LogLevel::Info, Some(vec![$($dec.to_string()),*]))
    };
    ([$($dec:expr),*], $msg:expr, $($arg:tt)*) => {
        $crate::log(&format!($msg, $($arg)*), $crate::LogLevel::Info, Some(vec![$($dec.to_string()),*]))
    };
    ($msg:expr) => {
        $crate::log($msg, $crate::LogLevel::Info, None)
    };
    ($msg:expr, $($arg:tt)*) => {
        $crate::log(&format!($msg, $($arg)*), $crate::LogLevel::Info, None)
    };
}

#[macro_export]
macro_rules! warn {
    ([$($dec:expr),*], $msg:expr) => {
        $crate::log($msg, $crate::LogLevel::Warn, Some(vec![$($dec.to_string()),*]))
    };
    ([$($dec:expr),*], $msg:expr, $($arg:tt)*) => {
        $crate::log(&format!($msg, $($arg)*), $crate::LogLevel::Warn, Some(vec![$($dec.to_string()),*]))
    };
    ($msg:expr) => {
        $crate::log($msg, $crate::LogLevel::Warn, None)
    };
    ($msg:expr, $($arg:tt)*) => {
        $crate::log(&format!($msg, $($arg)*), $crate::LogLevel::Warn, None)
    };
}

#[macro_export]
macro_rules! error {
    ([$($dec:expr),*], $msg:expr) => {
        $crate::log($msg, $crate::LogLevel::Error, Some(vec![$($dec.to_string()),*]))
    };
    ([$($dec:expr),*], $msg:expr, $($arg:tt)*) => {
        $crate::log(&format!($msg, $($arg)*), $crate::LogLevel::Error, Some(vec![$($dec.to_string()),*]))
    };
    ($msg:expr) => {
        $crate::log($msg, $crate::LogLevel::Error, None)
    };
    ($msg:expr, $($arg:tt)*) => {
        $crate::log(&format!($msg, $($arg)*), $crate::LogLevel::Error, None)
    };
}
