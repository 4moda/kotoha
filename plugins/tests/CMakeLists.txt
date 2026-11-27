add_executable(test_protocol test_protocol.cpp)
target_link_libraries(test_protocol PRIVATE kotoha::common kotoha_strict_warnings)
add_test(NAME kotoha_test_protocol COMMAND test_protocol)

add_executable(test_ring_buffer test_ring_buffer.cpp)
target_link_libraries(test_ring_buffer PRIVATE kotoha::common kotoha_strict_warnings)
add_test(NAME kotoha_test_ring_buffer COMMAND test_ring_buffer)

add_executable(test_ipc_client test_ipc_client.cpp)
target_link_libraries(test_ipc_client PRIVATE kotoha::common kotoha_strict_warnings)
# Shared Rust/C++ endpoint resolution cases (DESIGN.md Section 3.1).
target_compile_definitions(
    test_ipc_client
    PRIVATE
        KOTOHA_ENDPOINT_FIXTURE="${CMAKE_CURRENT_SOURCE_DIR}/../../tests/fixtures/endpoint_resolution_cases.tsv"
)
if(WIN32)
    target_compile_definitions(
        test_ipc_client
        PRIVATE
            WIN32_LEAN_AND_MEAN
            NOMINMAX
    )
endif()
add_test(NAME kotoha_test_ipc_client COMMAND test_ipc_client)

add_executable(test_wav_reader test_wav_reader.cpp)
target_link_libraries(test_wav_reader PRIVATE kotoha_strict_warnings)
target_compile_features(test_wav_reader PRIVATE cxx_std_20)
add_test(NAME kotoha_test_wav_reader COMMAND test_wav_reader)

add_executable(kotoha_mock_client kotoha_mock_client.cpp)
target_link_libraries(kotoha_mock_client PRIVATE kotoha::common kotoha_strict_warnings)

if(TARGET kotoha_mumble)
    add_executable(test_mumble_plugin_abi test_mumble_plugin_abi.cpp)
    target_include_directories(
        test_mumble_plugin_abi
        SYSTEM PRIVATE
            "${CMAKE_CURRENT_SOURCE_DIR}/../mumble/third_party/mumble"
    )
    target_compile_definitions(
        test_mumble_plugin_abi
        PRIVATE
            KOTOHA_MUMBLE_PLUGIN_PATH="$<TARGET_FILE:kotoha_mumble>"
    )
    if(WIN32)
        target_compile_definitions(
            test_mumble_plugin_abi
            PRIVATE
                WIN32_LEAN_AND_MEAN
                NOMINMAX
        )
    endif()
    target_link_libraries(
        test_mumble_plugin_abi
        PRIVATE
            kotoha::common
            kotoha_strict_warnings
            $<$<BOOL:${UNIX}>:${CMAKE_DL_LIBS}>
    )
    add_dependencies(test_mumble_plugin_abi kotoha_mumble)
    add_test(NAME kotoha_test_mumble_plugin_abi COMMAND test_mumble_plugin_abi)
endif()
