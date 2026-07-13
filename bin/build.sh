#!/bin/bash

LINE="--------------------------------------------------------------------------------"
DEFAULT_VERSION="0.0.1"
LOG_FILE_NAME=".tmp/build.log"
VERSION="$1"
BUILD_SUCCEDED=0

line() {
    printf "%s\n" "$LINE"
}

message() {
    printf "%s\n" "$1"
}

line
# Check if log file exist, otherwise creates it with needed path
if [ ! -f $LOG_FILE_NAME ]; then 
    message "Log file missing, creating it."
    IFS='/' read -ra LOG_FILE_NAME_PARTS <<< "$LOG_FILE_NAME"
    for ((i = 0; i < ${#LOG_FILE_NAME_PARTS[@]} - 1; i++)); do
        mkdir -p "${LOG_FILE_NAME_PARTS[i]}"
        cd "${LOG_FILE_NAME_PARTS[i]}"
    done
    cd - > /dev/null
    touch $LOG_FILE_NAME
else
    message "Log file found, proceeding."
fi

if [ ! -f $LOG_FILE_NAME ]; then 
    message "Log file creation error, aborting."
fi
line

line
# Check if the version is provided
if [ -z "$VERSION" ]; then
    message "Version number missing, using default: $DEFAULT_VERSION."
    VERSION=$DEFAULT_VERSION
fi
line

# Build the project
line
message "Building the project..."
# docker build -t ninoverse:$VERSION --progress=plain . 2>&1 | tee $LOG_FILE_NAME
# docker build -t ninoverse:$VERSION --progress=plain . > $LOG_FILE_NAME 2>&1
docker build -t ninoverse:$VERSION .
line

# Exit if the build encountered an error
line
message "Checking build results"
LOG_FILE_LAST_LINE_CONTENT=$(tail -n 1 $LOG_FILE_NAME)
if [[ $LOG_FILE_LAST_LINE_CONTENT == *"ERROR"* ]]; then
    message "Error occured while building the image, aborting."
    message "$LOG_FILE_LAST_LINE_CONTENT"
else
    message "Build completed successfull."
    BUILD_SUCCEDED=1
fi
line

if [ $BUILD_SUCCEDED -eq 1 ]; then
    line
    message "Turning down docker-compose..."
    docker-compose down
    line
    message "Running the project..."
    docker-compose up -d
    line
    message "Checking the status..."
    docker-compose ps
    line
fi

line
message "Memory occupied by docker:"
docker system df
line

line
message "Memory available on this pc:"
df -h | grep /home
line
