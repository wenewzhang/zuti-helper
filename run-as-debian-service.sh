#!/bin/bash

cargo build --release
sudo systemctl stop zuti-helper
sudo cp -a target/release/zuti-helper /usr/bin/.
sudo cp -a zuti-helper.service /usr/lib/systemd/system/zuti-helper.service
sudo systemctl daemon-reload
sudo systemctl start zuti-helper
sudo journalctl -xeu zuti-helper