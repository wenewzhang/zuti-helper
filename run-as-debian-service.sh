#!/bin/bash

cargo build --release
systemctl stop zuti-helper
cp -a target/release/zuti-helper /usr/bin/.
cp -a zuti-helper.service /usr/lib/systemd/system/zuti-helper.service
systemctl daemon-reload
systemctl start zuti-helper
journalctl -xeu zuti-helper