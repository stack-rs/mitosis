#!/bin/bash
RUST_LOG=netmito=debug ./target/debug/mito worker --user mitosis_admin --tags test1,test2 --heartbeat-interval 10s --polling-interval 5s  --groups mitosis_admin --lifetime 7d