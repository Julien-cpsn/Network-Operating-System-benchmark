#!/usr/bin/env bash
USER="$1"

set -e

sudo-g5k

sudo apt install -y \
    python3 \
    qemu-kvm \
    qemu-utils \
    libvirt-clients \
    libvirt-daemon-system \
    virtinst \
    ca-certificates \
    curl \
    gnupg2 \
    telnet \
    iproute2 \
    procps \
    numactl \
    linux-cpupower

# cargo lives in ~/.cargo/bin, which non-interactive job shells do not have in PATH
if [ -f "/home/${USER}/.cargo/env" ]; then
        . "/home/${USER}/.cargo/env"
fi

if ! command -v cargo >/dev/null 2>&1; then
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
        . "/home/${USER}/.cargo/env"
fi

# The VMs are started by the benchmark user and need KVM
if [ ! -w /dev/kvm ]; then
        sudo chmod o+rw /dev/kvm
fi

# vhost-net for the VirtIO NICs (taps are created through passwordless sudo)
sudo modprobe vhost_net
if [ ! -w /dev/vhost-net ]; then
        sudo chmod o+rw /dev/vhost-net
fi

echo 0 | sudo tee /sys/kernel/mm/ksm/run

# Host tuning, see claude/testbed-findings-link-backend.md (halt_poll_ns is left at its default:
# setting it to 0 slows down every guest wake-up)
# - SMT off: removes the STIBP cost on every VM exit and gives each vCPU a whole core
echo off | sudo tee /sys/devices/system/cpu/smt/control
# - fixed frequency and no deep C-states: no wake-up latency on idle cores
sudo cpupower frequency-set -g performance
sudo cpupower idle-set -D 2
# Recorded with the results
{
    uname -a
    cat /proc/cmdline
    grep . /sys/devices/system/cpu/vulnerabilities/*
    echo "smt: $(cat /sys/devices/system/cpu/smt/control)"
    echo "halt_poll_ns: $(cat /sys/module/kvm/parameters/halt_poll_ns)"
    numactl --hardware
} > host_config.txt 2>&1

cd "/home/${USER}/survey/Network-Operating-System-benchmark"

rm -rf experimentation/results/*

cargo run -- generate files --override

chmod +x run_experiment.sh
./run_experiment.sh

if [ ! -d "/home/${USER}/public/results" ]; then
        mkdir "/home/${USER}/public/results"
fi

cp -r "experimentation/results" "/home/${USER}/public/"
cp host_config.txt "/home/${USER}/public/results/"

echo DONE