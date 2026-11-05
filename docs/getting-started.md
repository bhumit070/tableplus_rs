# Getting started

## Install

Download the file for your platform from the
[latest release](https://github.com/bhumit070/tableplus_rs/releases/latest).

=== "macOS"

    Download the `.dmg` for your Mac (`macos-apple-silicon` for M-series
    chips, `macos-intel` for Intel), open it and drag **Tabula** onto
    **Applications**.

    Tabula isn't notarized by Apple yet, so the first launch needs
    **right-click → Open** (then **Open** again in the dialog). Or run:

    ```sh
    xattr -dr com.apple.quarantine /Applications/Tabula.app
    ```

=== "Windows"

    - `Tabula-…-windows-x86_64.msi` installs Tabula into Program Files with
      a Start menu entry, and upgrades an older install.
    - `Tabula-…-windows-x86_64.exe` is the whole app in one file; run it
      from anywhere without installing.

    Neither is code-signed yet, so SmartScreen may warn the first time:
    choose **More info → Run anyway**.

=== "Linux"

    On Debian or Ubuntu, install the `.deb`. It also installs the Vulkan
    loader and GNOME Keyring, which Tabula needs:

    ```sh
    sudo apt install ./Tabula-*-linux-x86_64.deb
    ```

    On other distributions, extract the `.tar.gz` and run `./tabula`. You'll
    need X11 or Wayland, Vulkan, and a Secret Service keyring (GNOME Keyring
    or KWallet) to save passwords.

## Add a connection

1. On the start screen, click **New Connection**.
2. Pick the database type and fill in host, port, user, password and
   database (or the file, for SQLite).
3. Optional:
    - **Tag** the environment (local → production). *Production* makes
      Tabula ask before anything that writes (see
      [safe mode](architecture/safety.md#safe-mode)).
    - **Require encrypted connection** turns on TLS, and **Verify server
      certificate** also checks the server's certificate and host name.
    - **Connect through an SSH tunnel**: password, private key, or
      ssh-agent (see [SSH tunnels](architecture/ssh.md)).
4. **Test** checks the settings; **Save** or **Connect** keeps them.

The password goes to the operating system's credential store (Keychain on
macOS, Credential Manager on Windows, Secret Service on Linux), never into
Tabula's files.

## Import from TablePlus

Click **Import** on the start screen:

- **From TablePlus export file…** reads a `.tableplusconnection` file
  (TablePlus → File → Export Connections). If it's password-protected,
  Tabula asks for that password. Database passwords in the export are
  imported too.
- **From TablePlus on this Mac** reads TablePlus's own connection list.
  That list doesn't contain passwords, so Tabula asks for each one on the
  first connect.

Connections already imported are skipped, and database types Tabula doesn't
support yet (such as SQL Server) are listed in the summary.

## Next

- [Features](features.md): what you can do once connected.
- [Keyboard shortcuts](shortcuts.md).
