cmake_minimum_required(VERSION 3.20.0)

# Pull in the siri_remote module (this repo's zephyr/ dir) without needing a
# west manifest entry, so the sample builds standalone.
set(ZEPHYR_EXTRA_MODULES ${CMAKE_CURRENT_SOURCE_DIR}/../..)

find_package(Zephyr REQUIRED HINTS $ENV{ZEPHYR_BASE})
project(siri_remote_print_events)

target_sources(app PRIVATE src/main.c)
