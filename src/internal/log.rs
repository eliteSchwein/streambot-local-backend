use chrono::Utc;
use owo_colors::OwoColorize;

fn timestamp() -> String {
    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

pub(crate) fn log(level: &str, message: &str, color: &str) {
    let prefix = format!("[{}] [{}]", level, timestamp());

    match color {
        "red" => println!("{} {}", prefix.bright_black(), message.red()),
        "green" => println!("{} {}", prefix.bright_black(), message.green()),
        "yellow" => println!("{} {}", prefix.bright_black(), message.yellow()),
        "cyan" => println!("{} {}", prefix.bright_black(), message.cyan()),
        "magenta" => println!("{} {}", prefix.bright_black(), message.magenta()),
        "grey" => println!("{} {}", prefix.bright_black(), message.bright_black()),
        _ => println!("{} {}", prefix.bright_black(), message),
    }
}

pub fn error(message: &str) {
    log("error", message, "red");
}

pub fn success(message: &str) {
    log("info", message, "green");
}

pub fn regular(message: &str) {
    log("info", message, "white");
}

pub fn notice(message: &str) {
    log("info", message, "magenta");
}

pub fn info(message: &str) {
    log("info", message, "cyan");
}

pub fn warn(message: &str) {
    log("warn", message, "yellow");
}

pub fn debug(message: &str) {
    log("debug", message, "grey");
}
