# siri_remote Zephyr module library.
if(CONFIG_SIRI_REMOTE)
  zephyr_library()
  zephyr_library_sources(
    src/siri_remote.c
    src/proto.c
  )
  zephyr_include_directories(include)
endif()
