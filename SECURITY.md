# Security Policy

## Supported Versions

Lanroam is in early development and has no release yet. Until the first release, security fixes land on `main` only; after that, they are provided for the latest released version.

| Version | Supported |
|---------|-----------|
| `main` / latest release | Yes |
| Older releases | No |

## Reporting A Vulnerability

If you discover a security vulnerability, please do **not** report it through a public issue, because doing so may expose the vulnerability before a fix is available.

Please report it privately by email: **zero9501@outlook.com**

When reporting, include as much of the following information as possible:

- Vulnerability type and impact scope
- Reproduction steps or proof of concept
- Affected versions or commit, and the operating systems of the devices involved

We will confirm the report as soon as possible and disclose it publicly after a fix has been released.

## Scope

Lanroam forwards keystrokes and injects input into other computers: whatever you type while controlling a device — passwords included — crosses the network, and a device that is controlled can be made to do anything its user could. Findings in these areas are especially welcome:

- **Joining a desk group** — anything that gets a device into a group without the PIN shown on a member's screen, weakens the PIN check (it is a SPAKE2 exchange bound to both TLS certificate fingerprints, with 3 attempts and a 30-second cooldown), or lets a relay between two devices join in their place
- **Membership** — a removed device getting back in without a new join, or a non-member being treated as a member
- **Transport** — weaknesses in the mutually authenticated TLS 1.3 (QUIC) connections, or any path where input, layout or membership data crosses the network without them
- **Input handling** — input from a non-member being injected, keys left held down after control ends, or a controlled device unable to take control back with its own keyboard and mouse
- **Protocol parsing** — anything remotely reachable that can crash, hang or exhaust the resources of a node on the same LAN

## Known Limitations

These are documented behaviours rather than vulnerabilities, but they define the security boundary:

- **Members of a desk group trust each other fully.** Any member can control any other member's keyboard and mouse, let new devices in and remove existing ones; membership records are not signed. Only join groups made of your own devices.
- **Removal reaches only members that are online.** A member that is offline when a device is removed still accepts that device until it next connects to a member that knows.
- **Anyone on the LAN can reach the port.** A device that is not a member can only ask to join (which pops up a PIN on the member it asked, one request at a time) or ping; anything else is rejected right after the TLS handshake, at the application layer.
- **Discovery is public on the LAN.** mDNS announces each device's name, ID, certificate fingerprint, platform, OS version and desk group ID in clear text.
- **The identity key is stored unencrypted** in the data directory (`~/.lanroam` by default), readable only by the owner on macOS and Linux, and protected by the per-user profile's permissions on Windows. Anyone with access to your user account can impersonate the device.
- **Secure input is out of reach.** On Windows, the lock screen, UAC prompts and windows running as administrator cannot be controlled remotely; on macOS, secure input (password fields, terminals with Secure Keyboard Entry) stops the keyboard from being captured.
- **Development builds are not signed or notarized**, so their authenticity cannot be verified through the OS.
