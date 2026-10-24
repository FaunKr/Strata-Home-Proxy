# One-time setup

To tick off before the proxy runs on the NAS. Keep the order: first
check that standby and waking work at all, then Strata, then the network.

## 1. BIOS (ASRock B550 Phantom Gaming 4)

The menu names may differ slightly depending on the BIOS version.

- [ ] **Advanced → ACPI Configuration → PCIE Devices Power On** to *Enabled* (this is Wake on LAN for the onboard network adapter)
- [ ] **Advanced → ACPI Configuration → Suspend to RAM** to *Enabled* or *Auto* (standby S3)
- [ ] **Advanced → PCI Configuration → Above 4G Decoding** and **Re-Size BAR Support** to *Enabled* (Resizable BAR for games)
- [ ] **Boot → Fast Boot** to *Disabled*, if waking does not work later (on some boards the network adapter otherwise stays off)

## 2. Windows

In a PowerShell as administrator:

```powershell
# Hibernation off, so that "Sleep" really is standby (saves ~64 GB on the NVMe)
powercfg -h off

# Check which sleep states the system supports: "Standby (S3)" must be among them
powercfg /a

# Show network adapter, name and MAC address
Get-NetAdapter | Where-Object Status -eq Up | Format-Table Name, InterfaceDescription, MacAddress, LinkSpeed

# Turn on Wake on Magic Packet (name from the line above, usually "Ethernet")
Set-NetAdapterPowerManagement -Name "Ethernet" -WakeOnMagicPacket Enabled
Get-NetAdapterAdvancedProperty -Name "Ethernet" | Where-Object DisplayName -match "Wake|Magic|Energy|EEE"
```

- [ ] `powercfg /a` shows **Standby (S3)** as available. If it only shows *Modern Standby (S0)*: report it, then the plan has to be adjusted.
- [ ] In the network adapter's advanced properties: *Wake on Magic Packet* = Enabled, *Energy Efficient Ethernet* or *Green Ethernet* = Disabled (otherwise often prevents waking)
- [ ] Device Manager → network adapter → Power Management: "Allow this device to wake the computer" and "Only allow a magic packet …" ticked
- [ ] Control Panel → Power Options → "Choose what the power buttons do" → **fast startup off**
- [ ] Power Options: Windows' automatic standby to *Never* (the proxy takes this over later)
- [ ] MAC address noted: `__-__-__-__-__-__`

## 3. Network

- [ ] PC on a **LAN cable** to the router or switch (Wi-Fi does not wake reliably)
- [ ] Assign a **fixed IP** to the PC in the router (DHCP reservation): `192.168.__.__`
- [ ] NAS and PC in the **same network** (same address range, no guest Wi-Fi/VLAN in between)
- [ ] Broadcast address of the network noted, usually the IP with `.255` at the end: `192.168.__.255`

## 4. Test waking by hand

Before anything else is built, this has to work:

1. Send the PC to standby via Start → Power → *Sleep*. Fans off, power LED blinks or is off.
2. Send a wake packet from the NAS. The proxy's image is enough for this,
   Strata does not have to run yet. Via SSH on the NAS (adjust MAC and
   broadcast):
   ```sh
   docker run --rm --network host \
     -e PC_MAC=AA-BB-CC-DD-EE-FF -e WOL_ADDR=192.168.1.255:9 \
     ghcr.io/faunkr/strata-home-proxy:latest wake
   ```
3. Time how long until the screen is back.

- [ ] PC wakes up, time: ____ s
- [ ] It works five times in a row

If it does not work: check the BIOS items from step 1 and *Energy Efficient Ethernet*. If that does not help, use `powercfg -lastwake` to see what Windows reports.

## 5. Strata

Easy way, once proxy and PC service are running: web UI, area
**Strata**. "Set up Strata" wakes the PC and installs the Strata Terminal with
Strata's own `install.ps1` and the engine for the PC. The page then offers
the models of Strata's own list, with Strata's recommendation for this PC
preselected. "Load model" downloads the first one, starts Strata with it and
enters the Strata key in Strata and the proxy. The skill `local-agent` needs
more context than the default (see its README).

- [ ] *Strata* area: "Set up Strata" runs through, then "Load model" loads the first model on the RX 6800 XT
- [ ] "Run benchmark" (tab *Version & benchmark*) gives tokens/s: ____
- [ ] `curl http://<PC-IP>:8080/health` from the NAS returns `200`
- [ ] Standby test: put the PC to sleep, wake it, then query `/health` once more. If Strata no longer answers afterwards, note it (that is what the watchdog in the PC service is meant for)

By hand it goes like this: install the Strata Terminal (`irm https://github.com/Niko1221/Strata/releases/latest/download/install.ps1 | iex`),
then `strata launch` with `--host 0.0.0.0`, port **8080** and an **API key**, then enter the key in the web UI under *Strata → Connection*.

## 6. Firewall on the PC

The install command of the PC service does this: it opens Strata's port only
for the NAS (rule *Strata from the NAS*), so a new Strata program after an
update needs no Windows prompt. Without the PC service, open port 8080 only for
the NAS (adjust IP):

```powershell
New-NetFirewallRule -DisplayName "Strata only from the NAS" -Direction Inbound -Protocol TCP -LocalPort 8080 -RemoteAddress 192.168.1.10 -Action Allow
```

- [ ] Rule created, older, more open rules for 8080 removed if any
- [ ] Reachable from the NAS, no longer from another computer

## 7. Switch over clients

Only once the proxy runs on the NAS (see README):

- [ ] Open `http://<NAS-IP>:8080/admin/`, log in with `admin` (or `UI_PASSWORD`) and set your own password
- [ ] *PC* area: *Create install command* and paste the command into a PowerShell on the PC ([docs/pc-service.md](pc-service.md))
- [ ] The checklist *Finish setup* on the *Overview* has disappeared (Strata address, MAC, PC service, API key and `DATA_DIR` are there)
- [ ] The *Overview* shows PC and Strata correctly; *Sleep now* and *Wake PC* work

What the web UI saves is stored in `/data` and applies right away. It takes precedence over
the environment variables, which in turn take precedence over the built-in defaults.
The only exception is the MAC address: if the PC service reports one, that one
always applies. With *Reset* next to a field, environment or default applies again.

Forgot password: delete `/data/admin-password.json` and restart the container.
After that `admin` (or `UI_PASSWORD`) applies again, all old
logins are invalid and a new password is required at the first login.

- [ ] Under *Access* create a separate key for each client and copy it right away; it is not shown again
- [ ] Set up the skill `local-agent` ([tools/local-agent/README.md](../tools/local-agent/README.md)): its own key from *Access* and the Strata settings from the README; `local-agent check` is green
- [ ] Skill's compaction proven on real hardware: the long test run from the README shows `context compacted` and ends without 400
- [ ] Claude Code directly against Strata (if used): `ANTHROPIC_BASE_URL=http://<NAS-IP>:8080` and `ANTHROPIC_API_KEY=<key from the web UI>` (goes out as `x-api-key`)
- [ ] Open WebUI (if used): *Admin Settings → Connections → OpenAI API*, URL `http://<NAS-IP>:8080/v1`, as key the key from the web UI

Without a valid key the proxy answers with `401` and does not wake the PC. The
Strata key stays on the NAS; clients no longer need it.
