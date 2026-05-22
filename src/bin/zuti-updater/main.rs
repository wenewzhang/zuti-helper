use std::fs;
use std::path::Path;

fn init_logger() {
    let log_dir = "/var/log/zuti";
    if !Path::new(log_dir).exists() {
        fs::create_dir_all(log_dir).expect("Failed to create log directory");
    }

    let log_file = format!("{}/zuti-updater.log", log_dir);

    let target = Box::new(
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_file)
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

fn main() {
    init_logger();
    log::info!("zuti-updater started");
}
