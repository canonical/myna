cmake_minimum_required(VERSION 3.16)
project(myna-fcitx5 LANGUAGES CXX)

include(GNUInstallDirs)

if(NOT DEFINED MYNA_FCITX5_VERSION)
    execute_process(
        COMMAND "${CMAKE_CURRENT_SOURCE_DIR}/../../dev/version.sh"
        OUTPUT_VARIABLE MYNA_FCITX5_VERSION
        OUTPUT_STRIP_TRAILING_WHITESPACE
        RESULT_VARIABLE version_result
    )
    if(NOT version_result EQUAL 0 OR MYNA_FCITX5_VERSION STREQUAL "")
        set(MYNA_FCITX5_VERSION "unknown")
    endif()
endif()

add_library(myna-fcitx5 MODULE
    src/myna-fcitx5.cpp
)

find_package(Fcitx5Core REQUIRED)

target_compile_features(myna-fcitx5 PRIVATE cxx_std_20)
target_link_libraries(myna-fcitx5 PRIVATE Fcitx5::Core)
set_target_properties(myna-fcitx5 PROPERTIES
    PREFIX ""
    OUTPUT_NAME "myna_fcitx5"
)
if(Fcitx5Core_VERSION VERSION_GREATER_EQUAL "5.1.12")
    target_compile_definitions(myna-fcitx5 PRIVATE MYNA_FCITX5_HAVE_ADDON_FACTORY_V2)
endif()

install(TARGETS myna-fcitx5
    DESTINATION "${FCITX_INSTALL_ADDONDIR}"
)

configure_file(
    data/myna_fcitx5.conf.in
    "${CMAKE_CURRENT_BINARY_DIR}/myna_fcitx5.conf"
    @ONLY
)
install(
    FILES "${CMAKE_CURRENT_BINARY_DIR}/myna_fcitx5.conf"
    DESTINATION "${FCITX_INSTALL_PKGDATADIR}/addon"
)

install(
    FILES data/fcitx5.input-method.json
    DESTINATION "${CMAKE_INSTALL_DATADIR}/myna/input-methods"
)
