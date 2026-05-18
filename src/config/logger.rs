use std::fs;
use std::path::Path;

pub fn init_logger() {
    // 确保日志目录存在
    let log_dir = "/var/log/zuti";
    if !Path::new(log_dir).exists() {
        fs::create_dir_all(log_dir).expect("Failed to create log directory");
    }

    let log_file = format!("{}/zuti-helper.log", log_dir);

    // 配置 env_logger，输出到文件
    let target = Box::new(fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_file)
        .expect("Failed to open log file"));

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
