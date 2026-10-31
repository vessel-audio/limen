cmake_minimum_required(VERSION 3.16)
project(limen_fcitx5 LANGUAGES CXX)

find_package(PkgConfig REQUIRED)
pkg_check_modules(FCITX5_CORE REQUIRED IMPORTED_TARGET Fcitx5Core>=5.1.0)
pkg_check_modules(FCITX5_UTILS REQUIRED IMPORTED_TARGET Fcitx5Utils>=5.1.0)
pkg_check_modules(FCITX5_MODULE REQUIRED IMPORTED_TARGET Fcitx5Module>=5.1.0)

add_library(limenfcitx5 MODULE limenfcitx5.cpp)
target_compile_features(limenfcitx5 PRIVATE cxx_std_20)
target_link_libraries(
    limenfcitx5
    PRIVATE
    PkgConfig::FCITX5_CORE
    PkgConfig::FCITX5_UTILS
    PkgConfig::FCITX5_MODULE
)
set_target_properties(
    limenfcitx5
    PROPERTIES
    PREFIX "lib"
    OUTPUT_NAME "limenfcitx5"
)

if(NOT LIMEN_DATA_HOME)
    set(LIMEN_DATA_HOME "$ENV{HOME}/.local/share")
endif()
set(LIMEN_LIBRARY_DIR "${LIMEN_DATA_HOME}/limen/fcitx5")
set(LIMEN_LIBRARY_PATH "${LIMEN_LIBRARY_DIR}/liblimenfcitx5")
configure_file(limenfcitx5.conf.in limenfcitx5.conf @ONLY)

install(TARGETS limenfcitx5 LIBRARY DESTINATION "${LIMEN_LIBRARY_DIR}")
install(
    FILES "${CMAKE_CURRENT_BINARY_DIR}/limenfcitx5.conf"
    DESTINATION "${LIMEN_DATA_HOME}/fcitx5/addon"
)
