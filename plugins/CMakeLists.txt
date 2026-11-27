cmake_minimum_required(VERSION 3.20)

project(
    kotoha_plugins
    VERSION 0.1.0
    DESCRIPTION "Kotoha C++20 voice-chat IPC plugins"
    LANGUAGES CXX
)

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)
set(CMAKE_EXPORT_COMPILE_COMMANDS ON)

option(KOTOHA_BUILD_TESTS "Build Kotoha C++ unit tests" ON)
option(KOTOHA_WARNINGS_AS_ERRORS "Treat compiler warnings as errors" OFF)

find_package(Threads REQUIRED)

add_library(kotoha_strict_warnings INTERFACE)
if(MSVC)
    target_compile_options(
        kotoha_strict_warnings
        INTERFACE
            /W4
            /permissive-
            /utf-8
            $<$<BOOL:${KOTOHA_WARNINGS_AS_ERRORS}>:/WX>
    )
else()
    target_compile_options(
        kotoha_strict_warnings
        INTERFACE
            -Wall
            -Wextra
            -Wpedantic
            $<$<BOOL:${KOTOHA_WARNINGS_AS_ERRORS}>:-Werror>
    )
endif()

add_subdirectory(common)

if(EXISTS "${CMAKE_CURRENT_SOURCE_DIR}/mumble/CMakeLists.txt")
    add_subdirectory(mumble)
endif()

if(KOTOHA_BUILD_TESTS)
    enable_testing()
    add_subdirectory(tests)
endif()
