#!/bin/bash
RUST_LOG=netmito=debug ./target/debug/mito coordinator --heartbeat-timeout 20s
# ./target/debug/mito coordinator
