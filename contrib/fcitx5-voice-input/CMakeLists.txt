cmake_minimum_required(VERSION 3.16)
project(voice_input_fcitx5 LANGUAGES CXX)

find_package(PkgConfig REQUIRED)
pkg_check_modules(FCITX5_CORE REQUIRED IMPORTED_TARGET Fcitx5Core>=5.1.0)
pkg_check_modules(FCITX5_UTILS REQUIRED IMPORTED_TARGET Fcitx5Utils>=5.1.0)
pkg_check_modules(FCITX5_MODULE REQUIRED IMPORTED_TARGET Fcitx5Module>=5.1.0)

add_library(voiceinputfcitx5 MODULE voiceinputfcitx5.cpp)
target_compile_features(voiceinputfcitx5 PRIVATE cxx_std_20)
target_link_libraries(
    voiceinputfcitx5
    PRIVATE
    PkgConfig::FCITX5_CORE
    PkgConfig::FCITX5_UTILS
    PkgConfig::FCITX5_MODULE
)
set_target_properties(
    voiceinputfcitx5
    PROPERTIES
    PREFIX "lib"
    OUTPUT_NAME "voiceinputfcitx5"
)

if(NOT VOICE_INPUT_DATA_HOME)
    set(VOICE_INPUT_DATA_HOME "$ENV{HOME}/.local/share")
endif()
set(VOICE_INPUT_LIBRARY_DIR "${VOICE_INPUT_DATA_HOME}/voice-input/fcitx5")
set(VOICE_INPUT_LIBRARY_PATH "${VOICE_INPUT_LIBRARY_DIR}/libvoiceinputfcitx5")
configure_file(voiceinputfcitx5.conf.in voiceinputfcitx5.conf @ONLY)

install(TARGETS voiceinputfcitx5 LIBRARY DESTINATION "${VOICE_INPUT_LIBRARY_DIR}")
install(
    FILES "${CMAKE_CURRENT_BINARY_DIR}/voiceinputfcitx5.conf"
    DESTINATION "${VOICE_INPUT_DATA_HOME}/fcitx5/addon"
)
