#!/bin/bash

# SFTP 上传脚本：将指定文件上传到 root@192.168.3.203:/root/zuti/

REMOTE_HOST="root@192.168.3.100"
REMOTE_DIR="/root/zuti-helper"
LOCAL_FILE="$1"

# 检查是否提供了文件参数
if [ -z "$LOCAL_FILE" ]; then
    echo "用法: $0 <本地文件路径>"
    exit 1
fi

# 检查本地文件是否存在
if [ ! -f "$LOCAL_FILE" ]; then
    echo "错误：本地文件 $LOCAL_FILE 不存在"
    exit 1
fi

# 拼接远程完整路径：REMOTE_DIR + LOCAL_FILE
REMOTE_PATH="${REMOTE_DIR}/${LOCAL_FILE}"

# 获取远程目录路径（去掉文件名）
REMOTE_PARENT=$(dirname "$REMOTE_PATH")

echo "正在上传 $LOCAL_FILE 到 $REMOTE_HOST:$REMOTE_PATH ..."

# 使用 sftp 上传文件
sftp "$REMOTE_HOST" << EOF
mkdir -p $REMOTE_PARENT
put $LOCAL_FILE $REMOTE_PATH
bye
EOF

if [ $? -eq 0 ]; then
    echo "上传成功"
else
    echo "上传失败"
    exit 1
fi
