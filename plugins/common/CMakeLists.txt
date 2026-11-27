add_library(
    kotoha_common
    STATIC
        src/protocol.cpp
        src/ipc_client.cpp
)

add_library(kotoha::common ALIAS kotoha_common)

target_include_directories(
    kotoha_common
    PUBLIC
        "${CMAKE_CURRENT_SOURCE_DIR}/include"
)

target_compile_features(kotoha_common PUBLIC cxx_std_20)
set_target_properties(kotoha_common PROPERTIES POSITION_INDEPENDENT_CODE ON)

target_link_libraries(
    kotoha_common
    PUBLIC
        Threads::Threads
    PRIVATE
        kotoha_strict_warnings
)

if(WIN32)
    target_compile_definitions(
        kotoha_common
        PRIVATE
            WIN32_LEAN_AND_MEAN
            NOMINMAX
    )
    # OpenProcessToken / GetTokenInformation / EqualSid (named pipe server identity check).
    target_link_libraries(kotoha_common PRIVATE advapi32)
endif()
