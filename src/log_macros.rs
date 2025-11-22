#[macro_export]
macro_rules! info_log {
    ($($arg:tt)*) => {
        eprintln!("\x1b[38;2;0;200;0m INFO | \x1b[0m {}", format!($($arg)*))
    };
}

#[macro_export]
macro_rules! warn_log {
    ($($arg:tt)*) => {
        eprintln!("\x1b[38;2;255;180;70m WARN | \x1b[0m {}", format!($($arg)*))
    };
}

#[macro_export]
macro_rules! error_log {
    ($($arg:tt)*) => {
        eprintln!("\x1b[38;2;255;80;80m ERROR| \x1b[0m {}", format!($($arg)*))
    };
}