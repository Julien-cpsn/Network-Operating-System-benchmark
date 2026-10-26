> [!WARNING]
> This is a PhD project, and it is still very WIP.

# Router OS benchmark

Benchmark and plot performances for router and network OSes.

## Prerequisites

- [QEMU](https://www.qemu.org/download/) (`qemu-system-x86_64` and `qemu-img`, KVM recommended)
- `telnet` (used to drive the VM serial consoles)
- [flent](https://flent.org/intro.html)
- [Rust with Cargo](https://rust-lang.org/fr/tools/install/)
- [uv](https://github.com/astral-sh/uv)
- `virt-builder` from libguestfs

## How to use

### 1. Create custom Debian guest

> [!NOTE]
> This is guest is needed for running [flent](https://flent.org/) ([iperf3](https://github.com/esnet/iperf), [netperf](https://github.com/HewlettPackard/netperf), [fping](https://fping.org/), [irtt](https://github.com/heistp/irtt)) and [xfr](https://github.com/lance0/xfr) experiments through the network.

```shell
virt-builder debian-13 \
  --output debian-trixie.qcow2 \
  --format qcow2 \
  --size 6G \
  --root-password password:debian \
  --upload sources.list:/etc/apt/sources.list \
  --install "curl,netperf,iperf3,fping,irtt,htop,net-tools,flent" \
  --run-command "curl -LO https://github.com/lance0/xfr/releases/latest/download/xfr-x86_64-unknown-linux-musl.tar.gz" \
  --run-command "tar xzf xfr-*.tar.gz && mv xfr /usr/local/bin/"
```

Then fill the `.env` file. You can use the `.env.example` as a starter. Only `GUEST_IMAGE_PATH` is required:

| Variable           | Default               | Description                                                                     |
|--------------------|-----------------------|---------------------------------------------------------------------------------|
| `GUEST_IMAGE_PATH` |                       | Debian guest image created above                                                |
| `QEMU_BINARY`      | `qemu-system-x86_64`  | QEMU system emulator                                                            |
| `QEMU_IMG_BINARY`  | `qemu-img`            | Used to create the linked clones                                                |
| `QEMU_WORK_DIR`    | `experimentation/vms` | VM disks, pidfiles, QEMU logs and command lines. Wiped before each experiment   |
| `QEMU_ENABLE_KVM`  | `true`                | Without KVM, VMs are emulated and routers use `-cpu max` instead of `-cpu host` |

### 2. Create an experiments

Fulfil the experiment data needed in the following files:

```
./experimentation/network_stacks.toml
./experimentation/operating_systems.toml
./experimentation/routing_stacks.toml
./experimentation/test_batches.toml
./experimentation/topologies.toml
```

Run the following command to generate experiment files:

```shell
cargo run -- generate files
```

Then you should see all possible experiments created in `./experimentation/experiments/` 

### 3. Run the experiments

No server is needed: the tool starts the QEMU VMs itself.

```shell
cargo run -- run --os SONiC --topology Dumbbell --protocol RIP --nic e1000 --test-batch "Basic RRUL at start" --resources hr
```

The command will run every experiment for SONiC OS, on Dumbbell topology, using RIP protocol, using E1000 NIC, using high resources, and the "Basic RRUL at start" test.
By removing an argument, the program will loop over all of its possible values. For example, by removing `--nic e1000`, the program with run the experiments with `rtl8139`, then with `e1000`, and then with `virtio-net-pci`. 

Results are stored in `./experimentation/results/`

#### How VMs are run

Each node of the topology is a `qemu-system-x86_64` process:

- **Disks**: linked clones, i.e. qcow2 overlays (`hda_disk.qcow2`, ...) backed by the untouched images of `operating_systems.toml` (routers, IDE) or `GUEST_IMAGE_PATH` (guests, virtio-scsi)
- **Links**: each physical link is a point-to-point UDP tunnel between two QEMU `socket` netdevs on `127.0.0.1` (UDP ports 10000-20000), unconnected adapters are left unplugged
- **Link failures**: the link is cut on both ends through QMP (`set_link ... up=false`), so both NICs lose carrier and no frame crosses the link anymore
- **Consoles**: the serial console is a telnet server on `127.0.0.1` (TCP ports 5000-10000, printed in the logs as `Telnet console: telnet 127.0.0.1 <port>`)
- **Control**: a QMP server per VM, used for link failures and to power the VM off

The PCI device order is the one GNS3 2.2 used, so the guests and routers keep the same interface names (e.g. `ens4` for the guests).
For each VM, `experimentation/vms/<experiment>/<node>/` contains `qemu_command.sh` (the exact command line, can be run again by hand), `qemu.log` and `qemu.pid`.
VMs left running (`--no-stop`, crash) are stopped at the start of the next experiment, and on Ctrl-C.

The backend has an integration test (needs QEMU, boots two empty VMs and checks PCI layout, overlays, links and shutdown): `cargo test qemu -- --ignored`

### 4. Pre-compute the results

```shell
cargo run -- compute
```

### 5. Plot the results

Then you can plot the results:

```shell
cargo run -- plot --os SONiC --nic e1000 --test-batch "Basic RRUL at start" --resources hr
```

The plots are then stored in `./experimentation/plots/` 

## Plot example

WIP

## Create ONIE and/or modified portable images

1. Create a linked clone of the original image, so it stays untouched:
   ```shell
   qemu-img create -f qcow2 -F qcow2 -b /absolute/path/to/original.qcow2 work.qcow2
   ```
   (use `-F raw` for a `.img`/`.iso` image, add one clone per disk if the OS needs several)
2. Boot it with its serial console in your terminal:
   ```shell
   qemu-system-x86_64 -enable-kvm -cpu host -m 4096 -smp 4 -nographic \
     -drive file=work.qcow2,if=ide,index=0,media=disk,format=qcow2
   ```
3. Proceed to the OS installation or modification, then power the VM off from the OS
4. Make it portable (standalone, without backing file): `qemu-img convert -f qcow2 -O qcow2 work.qcow2 your-portable-image.qcow2`
5. Reference `your-portable-image.qcow2` in `experimentation/operating_systems.toml`

## Deploy in Grid'5000

### Setup the environment

```shell
mkdir /home/${USER}/survey
cd /home/${USER}/survey
git clone https://github.com/Julien-cpsn/Network-Operating-System-benchmark
cd Network-Operating-System-benchmark

#>>>> Edit the experimentation files and the .env file

cargo run -- generate files --override
```

### Deploy (automated)

The `deploy_g5k` command uploads the images of an OS and reserves a node for its experiment.
```shell
cargo run -- deploy_g5k <USER> <SITE> <OS> [--queue abaca] [--cluster grappe] [--walltime 3:00] [--script PATH] [--no-wait]
# Example
cargo run -- deploy_g5k jcaposie nancy FreeBSD --walltime 3
```

It asks for your Grid'5000 password (only used for the Grid'5000 API), then:
1. connects over SSH to `access.grid5000.fr` (SSH agent or `~/.ssh/id_*` key, host key checked against `~/.ssh/known_hosts`)
2. clears `~/<SITE>/survey/vm` and uploads the `images_path` files of `<OS>` (from `experimentation/operating_systems.toml`) to `~/<SITE>/survey/vm/<OS>`
3. submits a job running `/home/<USER>/run_benchmark.sh "<USER>" "<OS>"` (override with `--script`) and waits for it to terminate (unless `--no-wait`)

### Check and download results in

https://api.grid5000.fr/stable/sites/SITE/public/USERNAME/

## Others

### Available experiments

from `flent --list-tests`

```
  bursts                     :  Latency measurements under intermittent UDP bursts
  bursts_11e                 :  802.11e Latency measurements under intermittent UDP bursts
  cisco_5tcpup               :  RTT Fair Realtime Response Under Load
  cisco_5tcpup_2udpflood     :  Cisco 5TCP up + 2 6Mbit UDP
  cubic_bbr                  :  Cubic VS BBR smackdown
  cubic_cdg                  :  Cubic VS CDG smackdown
  cubic_dctcp                :  Cubic VS DCTCP smackdown
  cubic_ledbat               :  Cubic VS Ledbat smackdown
  cubic_ledbat_1             :  Cubic vs LEDBAT upload streams w/ping
  cubic_reno                 :  Cubic VS Reno smackdown
  cubic_westwood             :  Cubic VS Westwood
  dashtest                   :  DASH testing
  dslreports_8dn             :  8 down - dslreports dsl test equivalent
  http                       :  HTTP latency test
  http-1down                 :  HTTP get latency with competing TCP download stream
  http-1up                   :  HTTP get latency with competing TCP upload stream
  http-rrul                  :  HTTP get latency with competing RRUL test
  iterated_bidirectional     :  Iterated TCP bidirectional transfers example
  ledbat_cubic_1             :  Cubic vs LEDBAT upload streams w/ping
  ping                       :  Ping test (ICMP and UDP)
  qdisc-stats                :  Capture qdisc stats
  reno_cubic_westwood_cdg    :  Realtime Response Under Load (with different congestion control algs)
  reno_cubic_westwood_ledbat :  Realtime Response Under Load (with different congestion control algs)
  reno_cubic_westwood_lp     :  Realtime Response Under Load (with different congestion control algs)
  rrul                       :  Realtime Response Under Load
  rrul46                     :  Realtime Response Under Load - Mixed IPv4/6
  rrul46compete              :  Realtime Response Under Load - Mixed v4/v6 compete
  rrul_100_up                :  100 up vs 1 down - exclusively Best Effort
  rrul_50_down               :  50 down vs 1 up - exclusively Best Effort
  rrul_50_up                 :  50 up vs 1 down - exclusively Best Effort
  rrul_be                    :  Realtime Response Under Load - exclusively Best Effort
  rrul_be_iperf              :  Realtime Response Under Load - exclusively Best Effort (Iperf TCP)
  rrul_be_nflows             :  Realtime Response Under Load - Best Effort, configurable no of flows
  rrul_cs8                   :  Realtime Response Under Load CS8, one flow per CS/precedence level
  rrul_icmp                  :  Realtime Response Under Load - Best Effort, only ICMP ping
  rrul_noclassification      :  Realtime Response Under Load - no classification on data flows
  rrul_prio                  :  Realtime Response Under Load - Test Prio Queue
  rrul_torrent               :  Torrent-like competition
  rrul_up                    :  Realtime Response Under Load - upload only
  rrul_var                   :  Realtime Response Under Load - variable configurable streams
  rtt_fair                   :  RTT Fair Realtime Response Under Load
  rtt_fair4be                :  RTT Fair Realtime Response Under Load
  rtt_fair6be                :  RTT Fair Realtime Response Under Load
  rtt_fair_up                :  RTT Fair upstream only
  rtt_fair_var               :  RTT Fair - variable number of hosts
  rtt_fair_var_down          :  RTT Fair - variable number of hosts (download only)
  rtt_fair_var_mixed         :  RTT Fair - variable number of hosts (mixed up and down)
  rtt_fair_var_up            :  RTT Fair - variable number of hosts (upload only)
  sctp_vs_tcp                :  SCTP vs TCP
  tcp_12down                 :  TCP download - 12 streams w/ping
  tcp_12up                   :  TCP upload - 12 streams w/ping
  tcp_1down                  :  Single TCP download stream w/ping
  tcp_1up                    :  Single TCP upload stream w/ping
  tcp_1up_noping             :  Single TCP upload stream
  tcp_2down                  :  TCP download - 2 streams w/ping
  tcp_2up                    :  TCP upload - 2 streams w/ping
  tcp_2up_delay              :  Two TCP upload streams; 2nd stream started delayed
  tcp_2up_square             :  Two TCP upload streams; 2nd stream started delayed
  tcp_2up_square_westwood    :  Two TCP upload streams; 2nd stream started delayed
  tcp_4down                  :  TCP download - 4 streams w/ping
  tcp_4up                    :  TCP upload - 4 streams w/ping
  tcp_4up_squarewave         :  Four TCP upload streams; 2nd streams started delayed, cubic vs BBR
  tcp_50up                   :  TCP upload - 8 streams w/ping
  tcp_6down                  :  TCP download - 6 streams w/ping
  tcp_6up                    :  TCP upload - 6 streams w/ping
  tcp_8down                  :  TCP download - 8 streams w/ping
  tcp_8up                    :  TCP upload - 8 streams w/ping
  tcp_bidirectional          :  Bidirectional TCP streams w/ping
  tcp_download               :  TCP download stream w/ping
  tcp_ndown                  :  TCP download - N streams w/ping
  tcp_nup                    :  TCP upload - N streams w/ping
  tcp_upload                 :  TCP upload stream w/ping
  tcp_upload_1000            :  1000 up - exclusively Best Effort
  tcp_upload_prio            :  TCP upload stream w/ToS prio bits
  udp_flood                  :  UDP flood w/ping
  udp_flood_var_up           :  UDP flood w/ping - variable number of hosts
  udp_flood_var_up_stagger   :  UDP flood w/ping - variable number of hosts, staggered start
  voip                       :  VoIP one-way stream test
  voip-1up                   :  VoIP one-way stream test with competing TCP stream
  voip-rrul                  :  VoIP one-way stream test with competing RRUL test
```
