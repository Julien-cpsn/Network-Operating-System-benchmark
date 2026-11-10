#!/usr/bin/env bash
USER="$1"
COMMANDS_FILE="$2"

set -e

# Several nodes may run this script at the same time on the same NFS home: everything they write
# to the shared project directory is either per node or protected by a lock
PROJECT_DIR="/home/${USER}/survey/Network-Operating-System-benchmark"
NODE="$(hostname -s)"
HOST_CONFIG="${PROJECT_DIR}/host_config_${NODE}.txt"

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
} > "${HOST_CONFIG}" 2>&1

cd "${PROJECT_DIR}"

#rm -rf experimentation/results/*

# One node at a time: building and (re)writing the experiment files while another node reads them
flock experimentation/.generate.lock cargo run -- generate files --override

chmod +x "${COMMANDS_FILE}"
./"${COMMANDS_FILE}"

dest="/home/${USER}/public/results"
mkdir -p "$dest"

for d in experimentation/results/*/; do
  name=$(basename "$d")
  # Still being run by another node: its results are incomplete, that node copies them when it is done
  if [ -e "experimentation/locks/${name}.lock" ]; then
    echo "Skipping ${name}: still running on another node"
    continue
  fi
  rm -rf "${dest:?}/${name}"
  cp -r "${d%/}" "$dest/"
done

cp "${HOST_CONFIG}" "$dest/"

echo DONE