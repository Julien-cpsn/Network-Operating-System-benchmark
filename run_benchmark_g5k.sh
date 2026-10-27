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
    procps

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

echo 0 | sudo tee /sys/module/kvm/parameters/halt_poll_ns
echo 0 | sudo tee /sys/kernel/mm/ksm/run

cd "/home/${USER}/survey/Network-Operating-System-benchmark"

rm -rf experimentation/results/*

cargo run -- generate files --override

chmod +x run_experiment.sh
./run_experiment.sh

if [ ! -d "/home/${USER}/public/results" ]; then
        mkdir "/home/${USER}/public/results"
fi

cp -r "experimentation/results" "/home/${USER}/public/"

echo DONE