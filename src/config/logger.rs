use std::fs;
use std::path::Path;

fn build_logger(log_file: &str) {
    let target = Box::new(
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_file)
            .expect("Failed to open log file"),
    );

    env_logger::Builder::from_default_env()
        .target(env_logger::Target::Pipe(target))
        .filter_level(log::LevelFilter::Info)
        .format(|buf, record| {
            use std::io::Write;
            writeln!(
                buf,
                "[{} {} {}] {}",
                chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
                record.level(),
                record.target(),
                record.args()
            )
        })
        .init();
}

fn ensure_log_dir() -> String {
    let log_dir = "/var/log/zuti";
    if !Path::new(log_dir).exists() {
        fs::create_dir_all(log_dir).expect("Failed to create log directory");
    }
    log_dir.to_string()
}

pub fn init_logger() {
    let log_dir = ensure_log_dir();
    let log_file = format!("{}/zuti-helper.log", log_dir);
    build_logger(&log_file);
}

pub fn init_logger_for(name: &str) {
    let log_dir = ensure_log_dir();
    let log_file = format!("{}/{}.log", log_dir, name);
    build_logger(&log_file);
}
