add_library(
    kotoha_mumble
    SHARED
        src/plugin.cpp
)

set_target_properties(
    kotoha_mumble
    PROPERTIES
        CXX_VISIBILITY_PRESET hidden
        VISIBILITY_INLINES_HIDDEN ON
)

target_include_directories(
    kotoha_mumble
    SYSTEM PRIVATE
        "${CMAKE_CURRENT_SOURCE_DIR}/third_party/mumble"
)

target_compile_definitions(
    kotoha_mumble
    PRIVATE
        KOTOHA_PLUGIN_VERSION="${PROJECT_VERSION}"
        KOTOHA_PLUGIN_VERSION_MAJOR=${PROJECT_VERSION_MAJOR}
        KOTOHA_PLUGIN_VERSION_MINOR=${PROJECT_VERSION_MINOR}
        KOTOHA_PLUGIN_VERSION_PATCH=${PROJECT_VERSION_PATCH}
)

if(WIN32)
    target_compile_definitions(
        kotoha_mumble
        PRIVATE
            WIN32_LEAN_AND_MEAN
            NOMINMAX
    )
endif()

target_link_libraries(
    kotoha_mumble
    PRIVATE
        kotoha::common
        kotoha_strict_warnings
)

if(UNIX AND NOT APPLE)
    target_link_options(kotoha_mumble PRIVATE -Wl,--exclude-libs,ALL)
endif()
