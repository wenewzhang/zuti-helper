pub const SAMBA_PASSDB_PATH: &str = "/var/lib/samba/private/passdb.tdb";
pub const ZUTI_DB_PATH: &str = "/.data/zuti/db.sqlite";
pub const SQLITE_MIGRATIONS_DIR: &str = "/usr/share/zuti/migrations/";

pub const UPGRADE_FILES: &[&str] = &[
    "/etc/samba/conf.d/all-share.conf",
    "/etc/samba/conf.d/private.conf",
    "/etc/samba/conf.d/public.conf",
    "/.data/zuti/podman/",
];

pub const UPGRADE_MUST_COPY_FILES: &[&str] = &[
"/etc/systemd/network/",
"/etc/ssh/sshd_config",
];
    