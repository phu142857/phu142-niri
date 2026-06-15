# phu142-niri

Fork của [niri](https://github.com/niri-wm/niri) — compositor Wayland scrollable-tiling — với thêm **Stage Manager** (kiểu macOS Stage Manager).

- **Fork này:** [https://github.com/phu142857/phu142-niri](https://github.com/phu142857/phu142-niri)
- **Niri gốc:** [https://github.com/niri-wm/niri](https://github.com/niri-wm/niri)
- **Tài liệu niri gốc:** [https://niri-wm.github.io/niri/](https://niri-wm.github.io/niri/)

Mọi tính năng của niri gốc vẫn giữ nguyên. Phần dưới chỉ mô tả thêm Stage Manager, cách backup, cài đặt và cấu hình cho fork này.

---

## Stage Manager là gì?

Stage Manager chia màn hình thành hai vùng:


| Vùng             | Mô tả                                                                     |
| ---------------- | ------------------------------------------------------------------------- |
| **Main (stage)** | Ứng dụng chính; kích thước theo `proportion-horizontal` × 1920 và `proportion-vertical` × 1080 |
| **Cast strip**   | Các app khác hiển thị thumbnail trên cạnh màn hình (`stack-position`); click để đưa lên main      |


---

## Backup trước khi cài

Trước khi thay binary niri, nên sao lưu binary cũ và config:

```bash
# Tạo thư mục backup (có thể đổi ngày tùy ý)
mkdir -p ~/niri-backup-$(date +%Y%m%d)

# Backup binary niri đang dùng (đường dẫn phổ biến)
sudo cp -a "$(command -v niri)" ~/niri-backup-$(date +%Y%m%d)/niri 2>/dev/null \
  || echo "Không tìm thấy niri trong PATH"

# Backup config
cp -a ~/.config/niri ~/niri-backup-$(date +%Y%m%d)/config-niri

# (Tuỳ chọn) Backup session / unit systemd user
cp -a ~/.config/systemd/user/niri.service ~/niri-backup-$(date +%Y%m%d)/ 2>/dev/null || true
```

**Khôi phục niri gốc sau này:**

```bash
sudo install -m 755 ~/niri-backup-YYYYMMDD/niri /usr/local/bin/niri
# hoặc copy lại vào đúng path package manager đã cài (vd. /usr/bin/niri)
cp -a ~/niri-backup-YYYYMMDD/config-niri ~/.config/niri
```

---

## Cài đặt

### Yêu cầu build

- Rust ≥ 1.85 (`rustup` khuyến nghị)
- Thư viện hệ thống giống [hướng dẫn build niri gốc](https://niri-wm.github.io/niri/Getting-Started.html#building)

**Arch / CachyOS** (ví dụ):

```bash
sudo pacman -S --needed base-devel clang systemd-libs libgbm libxkbcommon \
  mesa libwayland libinput dbus seatd pipewire pango libdisplay-info
```

Tên package có thể khác một chút giữa các distro; xem mục Building trong wiki niri gốc nếu `cargo build` báo thiếu thư viện.

### Build và cài binary

```bash
git clone https://github.com/phu142857/phu142-niri.git
cd phu142-niri
cargo build --release
sudo install -m 755 target/release/niri /usr/local/bin/niri
```

Kiểm tra:

```bash
niri --version
which niri
```

### Chạy session

Giống niri gốc:

- Từ display manager: chọn **Niri** khi đăng nhập.
- Từ TTY: `niri-session` hoặc `niri --session`.

Fork **không** thay thế toàn bộ desktop environment — vẫn cần bar, launcher, portal, v.v. như khi dùng niri gốc.

---

## Cấu hình Stage Manager

Config niri nằm tại `~/.config/niri/config.kdl` (định dạng [KDL](https://kdl.dev)).

### Bật Stage Manager

Thêm block `stage-manager` trong `layout { }`:

```kdl
layout {
    center-focused-column "never"

    stage-manager {
        proportion-horizontal 0.825
        proportion-vertical 0.92
        stack-position "left"
        max-cast-groups 2
        thumb-scale 0.19

        auto-use-as-main true
        auto-use-as-main-delay-ms 0
    }
}
```

| Tuỳ chọn | Giá trị ví dụ | Ý nghĩa |
| -------- | ------------- | ------- |
| `proportion-horizontal` | `0.825` | Chiều rộng main = 0.825 × **1920** → **1584 px** |
| `proportion-vertical` | `0.92` | Chiều cao main = 0.92 × **1080** → **994 px** |
| `stack-position` | `"left"` | Cast strip bên trái (`right`, `top`, `bottom`) |
| `max-cast-groups` | `2` | Tối đa 2 nhóm thumbnail trên strip |
| `thumb-scale` | `0.19` | Kích thước thumbnail (0.1 – 0.3) |
| `auto-use-as-main` | `true` | Focus thumbnail (bàn phím) → tự lên main |
| `auto-use-as-main-delay-ms` | `0` | Không chờ; lên main ngay khi focus stack |

`proportion` (cũ) vẫn dùng được — gán cả ngang và dọc cùng một giá trị nếu không ghi riêng.

Nếu **không** có block `stage-manager`, compositor chạy layout niri gốc (không có cast strip).

### Phím tắt Stage Manager

Thêm vào `binds { }` (đã có sẵn trong `resources/default-config.kdl`):

```kdl
binds {
    // Cần bật layout.stage-manager ở trên
    Mod+G { stage-manager-toggle-main; }
    Mod+Shift+G { stage-manager-promote-parallel; }
}
```


| Phím          | Tác dụng                                                      |
| ------------- | ------------------------------------------------------------- |
| `Mod+G`       | Đổi focus giữa app trên main và app đang chọn trên cast strip |
| `Mod+Shift+G` | Đưa app từ stack lên main song song (tối đa 2 app trên main)  |


`Mod` mặc định là phím **Super** (Windows) khi chạy trên TTY.

### Tương tác chuột

- **Click thumbnail** trên cast strip → app đó lên main, thay app main hiện tại.
- **Kéo thumbnail** lên vùng main → cùng hiệu ứng thay thế / merge tùy vị trí thả.
- **`auto-use-as-main true`**: focus thumbnail trên stack (bàn phím) → app tự lên main sau `auto-use-as-main-delay-ms` (`0` = ngay lập tức). Giữ kích thước đã resize thủ công. Hover chuột không kích hoạt.

Config reload live: sửa `config.kdl` và lưu — niri tự nạp lại (trừ vài thay đổi cần restart session).

---

## Gỡ / quay lại niri gốc

1. Khôi phục binary từ thư mục backup (mục [Backup](#backup-trước-khi-cài)).
2. Trong `config.kdl`, xóa hoặc comment block `stage-manager { }` và các bind `stage-manager-`* nếu không dùng trên bản gốc.
3. Đăng xuất và đăng nhập lại session.

---

## Phát triển

```bash
cargo build
cargo test -p niri-config stage_manager
```

Stage Manager: `src/layout/stage_manager.rs`, tích hợp workspace tại `src/layout/workspace.rs`.

---

## Giấy phép

Giữ nguyên **GPL-3.0-or-later** như [niri gốc](https://github.com/niri-wm/niri/blob/main/LICENSE).