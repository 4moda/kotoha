add_executable(test_protocol test_protocol.cpp)
target_link_libraries(test_protocol PRIVATE kotoha::common kotoha_strict_warnings)
add_test(NAME kotoha_test_protocol COMMAND test_protocol)

add_executable(test_ring_buffer test_ring_buffer.cpp)
target_link_libraries(test_ring_buffer PRIVATE kotoha::common kotoha_strict_warnings)
add_test(NAME kotoha_test_ring_buffer COMMAND test_ring_buffer)

add_executable(test_ipc_client test_ipc_client.cpp)
target_link_libraries(test_ipc_client PRIVATE kotoha::common kotoha_strict_warnings)
if(WIN32)
    target_compile_definitions(
        test_ipc_client
        PRIVATE
            WIN32_LEAN_AND_MEAN
            NOMINMAX
    )
endif()
add_test(NAME kotoha_test_ipc_client COMMAND test_ipc_client)
