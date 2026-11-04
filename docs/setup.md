# Setup guide

This guide takes you from a PC that is always on to one that sleeps until an
app needs it. Keep the order: first make sure standby and waking work at all,
then install the proxy, then the PC service and Strata.

What you need is listed in the [README](../README.md#what-you-need). The
examples use these addresses; replace them with yours:

| What | Example |
| --- | --- |
| PC | `192.168.1.50` |
| NAS | `192.168.1.10` |
| Broadcast address of the network | `192.168.1.255` |
| MAC address of the PC | `AA-BB-CC-DD-EE-FF` |

## 1. BIOS or UEFI of the PC

Menu names differ between boards and BIOS versions; search for the terms in
bold. On ASRock boards the first two sit under *Advanced -> ACPI
Configuration*.

- **Wake on LAN** for the onboard network adapter: *Enabled*. Some boards call
  it *PCIE Devices Power On*, *Power On By PCI-E* or *Resume by LAN*.
- **Suspend to RAM** (standby S3): *Enabled* or *Auto*.
- **ErP** or **Deep Sleep**, if the board has it: *Disabled*. It cuts power to
  the network adapter in standby.
- **Fast Boot**: *Disabled*, if waking does not work later. On some boards the
  network adapter otherwise stays off.

Check: the BIOS saves the settings and Windows starts normally.

## 2. Windows

In a PowerShell as administrator:

```powershell
# Hibernation off, so that "Sleep" really is standby
powercfg -h off

# Which sleep states the PC supports: "Standby (S3)" must be among them
powercfg /a

# Network adapter, name and MAC address
Get-NetAdapter | Where-Object Status -eq Up | Format-Table Name, InterfaceDescription, MacAddress, LinkSpeed

# Wake on Magic Packet on (the name from the line above, usually "Ethernet")
Set-NetAdapterPowerManagement -Name "Ethernet" -WakeOnMagicPacket Enabled
Get-NetAdapterAdvancedProperty -Name "Ethernet" | Where-Object DisplayName -match "Wake|Magic|Energy|EEE"
```

Then:

- In the network adapter's advanced properties: *Wake on Magic Packet*
  *Enabled*; *Energy Efficient Ethernet* or *Green Ethernet* *Disabled* (it
  often prevents waking).
- Device Manager -> network adapter -> Power Management: tick *Allow this
  device to wake the computer* and *Only allow a magic packet to wake the
  computer*.
- Control Panel -> Power Options -> *Choose what the power buttons do*: turn
  off *fast startup*.
- Power Options: set Windows' own sleep timer to *Never*. The proxy decides
  when the PC sleeps.
- Note the MAC address from `Get-NetAdapter`.

Check: `powercfg /a` lists **Standby (S3)** as available. If it only lists
*Standby (S0 Low Power Idle)* (Modern Standby), Wake on LAN from standby is
often unreliable; look for an S3 option in the BIOS first.

## 3. Network

- Connect the PC with a **cable** to the router or switch. Wi-Fi does not wake
  reliably.
- Give the PC a **fixed IP** in the router (DHCP reservation).
- Put the NAS and the PC in the **same network**: the same address range, no
  guest network or VLAN between them. The wake packet is a broadcast and does
  not cross routers.
- Note the network's broadcast address: usually the PC's IP with `.255` at the
  end.

Check: `ping 192.168.1.50` from the NAS answers.

## 4. Test waking by hand

Before anything else, waking has to work.

1. Put the PC to sleep: Start -> Power -> *Sleep*. Fans go off, the power LED
   blinks or goes off.
2. On the NAS, over SSH, send a wake packet with the proxy's image (adjust MAC
   and broadcast address):
   ```sh
   docker run --rm --network host \
     -e PC_MAC=AA-BB-CC-DD-EE-FF -e WOL_ADDR=192.168.1.255:9 \
     ghcr.io/faunkr/strata-home-proxy:latest wake
   ```
3. Time how long it takes until the screen is back.

Check: the PC wakes five times in a row. If it does not, see
[The PC does not wake](#the-pc-does-not-wake).

## 5. Install the proxy on the NAS

The container needs `network_mode: host`, otherwise the wake packet does not
reach the home network. Its data (settings, password hash, API keys, history)
lives in `/data` inside the container; mount a directory there so it survives
updates. Every setting can also be changed later in the web UI, so the
environment only needs the basics.

The second service, `code-bench`, runs the code benchmark. Without it
everything else works the same; you can remove it from the files below.

### TrueNAS SCALE

1. Create a dataset or directory for the data, for example
   `/mnt/POOL/apps/strata-proxy`.
2. *Apps -> Discover Apps -> (three dots) -> Install via YAML*, and paste
   [deploy/truenas-app.yaml](../deploy/truenas-app.yaml). Name the app
   `strata-proxy`.
3. Adjust `STRATA_URL` (the PC's IP, port 8080), `PC_MAC`, `WOL_ADDR`, `TZ`
   and the volume path, then save.

For automatic updates with backup and rollback, see
[Automatic updates on TrueNAS](nas-updates.md) once everything runs.

### Docker Compose (any other system)

```sh
mkdir -p strata-proxy && cd strata-proxy
base=https://raw.githubusercontent.com/FaunKr/Strata-Home-Proxy/main/deploy
curl -fsSL -o docker-compose.yml "$base/docker-compose.yml"
curl -fsSL -o .env "$base/env.example"
nano .env                # set STRATA_URL, PC_MAC, WOL_ADDR and TZ
docker compose pull
docker compose up -d
docker compose logs -f
```

All settings are explained in [deploy/env.example](../deploy/env.example).
To update later: `docker compose pull && docker compose up -d`.

Check: `http://192.168.1.10:8080/admin/` opens the login page.

## 6. First login

1. Open `http://192.168.1.10:8080/admin/` and log in with `admin` (or the
   value of `UI_PASSWORD`, if you set it).
2. Set your own password, at least 8 characters.

The *Overview* now shows the checklist *Finish setup*. It lists what is still
missing and links to the right place: the next steps tick it off.

Check: the *Overview* shows the PC as awake or asleep. Press *Wake PC* while
it sleeps; it wakes.

## 7. Install the PC service

The PC service runs in the background on the PC. It reports the PC's state
and MAC address, starts and watches Strata, switches gaming mode and puts the
PC to sleep when the proxy asks and nobody is using it.

1. In the web UI, open the *PC* area and press *Create install command*.
2. Paste the command into a PowerShell on the PC. Windows asks once for
   administrator rights.

The command is valid once, for 15 minutes. It installs the PC service, makes
it start at login, opens its port and Strata's port in the Windows firewall
only for the NAS, creates a desktop shortcut for gaming mode and registers
the PC with the proxy. Details: [PC service](pc-service.md).

The PC service runs in your Windows user account, not as a Windows service:
only this way does it see mouse and keyboard. After a restart of the PC,
someone has to log in, or Windows automatic login has to be set up.

Check: the *PC* area shows the PC service and its version, and *Finish setup*
ticks off *PC service* and *MAC address of the PC*.

## 8. Set up Strata

1. In the *Strata* area, press *Set up Strata*. It wakes the PC if needed and
   installs the Strata Terminal and the engine for the PC's GPU.
2. The page offers the models of Strata's own list, with Strata's
   recommendation for this PC preselected. Press *Load model*: it downloads
   the model, starts Strata with it and hands the Strata key to the proxy.
3. Optional: *Run benchmark* (tab *Version & benchmark*) measures the model's
   speed in tokens/s.

Check: the badge at the top shows Strata as ready.

Strata already on the PC from an older setup? The *Strata* area offers to
switch it to the Strata Terminal: [PC service](pc-service.md#switching-to-the-strata-terminal).

## 9. Create an API key and connect an app

1. In the *Access* area, create one API key per app. Copy it right away; it is
   not shown again.
2. Point the app at the proxy: [Connecting apps](clients.md) has opencode,
   Claude Code and Open WebUI.

Check: *Finish setup* has disappeared from the *Overview*, and a request from
the app gets an answer.

## 10. Test the whole round

1. Press *Sleep now* on the *Overview*. The PC goes to standby.
2. Send a request from an app. The PC wakes, Strata loads, the answer comes.
3. Leave the PC alone for 15 minutes. It goes back to standby by itself.

## Optional

- **Automatic updates on TrueNAS:** [nas-updates.md](nas-updates.md).
- **Power plug:** with a power plug that measures power between the PC and
  the wall, the *Overview* and the history show the PC's power and energy.
  Set its address in the *PC* area; its help text names the supported plugs.
- **Prometheus:** [metrics](metrics.md).
- **Time zone:** set `TZ` (for example `Europe/Berlin`) so that times of day
  and keep awake "until 8 am" are right.

## Troubleshooting

When the proxy answers instead of Strata, the header `x-strata-proxy-status`
says why. With curl: `curl -i http://192.168.1.10:8080/v1/models -H "Authorization: Bearer <key>"`.

| Symptom | Cause | Fix |
| --- | --- | --- |
| `401`, `unauthorized` | The app sends no API key or a wrong one. | Create a key under *Access* and set it in the app. |
| `401`, `strata_unauthorized` | Strata refused the proxy's Strata key. | Check that the PC service runs and is up to date (*PC* area); the PC service makes a new Strata key at every start of Strata. |
| `503`, `gaming` | Gaming mode is on. | Turn it off on the *Overview* or with the desktop shortcut. |
| `503`, `tuning` | Strata is tuning a model. | Wait until the tune ends. |
| `503`, `not_configured` | No Strata address is set. | Set it in the *Strata* area, tab *Connection*. |
| `503`, `no_mac` | The proxy does not know the PC's MAC address. | Set it in the *PC* area, or wake the PC once by hand so the PC service reports it. |
| `504`, `wake_timeout` | The PC did not wake within the wake timeout. | See [The PC does not wake](#the-pc-does-not-wake). |

### The PC does not wake

- Recheck the BIOS settings from step 1, especially Wake on LAN and ErP.
- Turn off *Energy Efficient Ethernet* or *Green Ethernet* on the network
  adapter.
- Check `WOL_ADDR`: the broadcast address of the PC's network, port 9.
- Make sure NAS and PC are in the same network and the proxy runs with
  `network_mode: host`.
- On the PC, `powercfg -lastwake` shows what Windows saw as the last wake
  source.

### The PC does not go to sleep

The *Overview* shows why: someone used mouse or keyboard in the last
10 minutes, Strata is computing, gaming mode or *Keep awake* is on, or a
request is still open. *Force sleep* puts it to sleep anyway.

### Forgot the web UI password

Delete `admin-password.json` in the data directory and restart the container.
Then `admin` (or `UI_PASSWORD`) applies again, all old logins are invalid, and
a new password is required at the first login.

### Settings are lost after a restart

The data directory is not mounted on `/data`. The *System* area warns about
this. Mount a directory as in step 5.
