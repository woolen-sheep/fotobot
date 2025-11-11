# 📸 Fotobot RS

Fotobot RS is a high-performance Telegram bot focused on lightning-fast EXIF extraction from uploaded photos. ⚡ Instead of downloading entire media files, it only streams the header bytes needed to decode metadata, keeping bandwidth usage low and response times snappy.

## ✨ Key Features
- **Header-only downloads:** Grabs just the crucial portion of each photo to read EXIF data without pulling the whole file.
- **20 MB limit workaround:** Uses a native MTProto client to bypass Telegram's standard 20 MB download cap for bots.
- **Rust-powered performance:** Built with Rust for reliability, safety, and top-notch speed under load.

## 🚀 How It Works
1. The bot listens for photo messages in Telegram chats.
2. It opens the file using MTProto, streaming only the initial bytes that carry EXIF information.
3. Metadata is parsed and returned to the chat almost instantly.

## 🛠️ Development
- `cargo build` to compile the project
- `cargo run` to launch the bot (ensure your Telegram API credentials and bot token are configured)

## 🧭 Systemd Service
- Copy `fotobot.service.example` to `/etc/systemd/system/fotobot.service` and adjust `User`, `Group`, `WorkingDirectory`, and `ExecStart` to match your setup.
- Replace the placeholder values in the `Environment=` lines with the real `BOT_TOKEN`, `TG_ID`, and `TG_HASH`.
- Build a release binary with `cargo build --release` and place the resulting executable somewhere on your host, e.g. `/usr/local/bin/fotobot_rs`.
- Reload systemd and start the service with:
	- `sudo systemctl daemon-reload`
	- `sudo systemctl enable --now fotobot`
- Check the status and logs via `sudo systemctl status fotobot` and `journalctl -u fotobot -f`.

## 🙏 Inspiration
This project is inspired by [M0gician/fotobot](https://github.com/M0gician/fotobot), bringing similar ideas to Rust with extra performance tuning.

## ⚠️ Disclaimer
This project is primarily built by an AI agent. Review the code and configuration to ensure it meets your production requirements before deploying.

## 📬 Feedback & Contributions
Issues and pull requests are welcome! Share ideas, report bugs, or tune performance further—every contribution helps this bot shine.
