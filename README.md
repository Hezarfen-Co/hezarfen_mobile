# Hezarfen Mobile

[Hezarfen Frontend](https://github.com/Hezarfen-Co/hezarfen_frontend) uygulamasını
[Tauri v2](https://v2.tauri.app) ile Android ve iOS'a paketleyen kabuk.

Web uygulamasının kaynak kodunda **tek satır değişiklik yok**. Frontend bu repoya
`frontend/` altında bir git submodule olarak bağlıdır.

---

## Neden bir gömülü proxy var?

Web uygulaması backend ile aynı origin'den servis edildiğini varsayıyor:

- `fetch("/api/...", { credentials: "same-origin" })`
- `<img src="/api/users/:id/avatar">` — görsel isteğini session cookie'si yetkilendiriyor
- `new EventSource("/api/chatbot/.../stream")`
- `` new WebSocket(`${proto}//${window.location.host}/api/exams/:id/attempt/ws`) `` (sınav odası, whiteboard)

Tarayıcı dağıtımında bu origin'i frontend reposundaki `server.ts` (Bun) sağlıyor.
Tauri'de webview `tauri://localhost` üzerinden yüklenseydi bunların hepsi kırılırdı:
`/api/...` uygulamanın kendi asset protokolüne düşerdi.

Bu yüzden `server.ts` uygulamanın içinde Rust olarak yeniden yazıldı
([`src-tauri/src/proxy.rs`](src-tauri/src/proxy.rs)): `127.0.0.1` üzerinde bir
loopback HTTP sunucusu paketlenmiş SPA'yı servis eder ve `/api/*` isteklerini
(HTTP **ve** WebSocket) backend'e proxy'ler. Webview bu adrese bakar, dolayısıyla
`window.location.host` = `127.0.0.1:18420` olur ve frontend hiçbir şeyin
değiştiğini fark etmez.

```
webview → http://127.0.0.1:18420
  ├── /                → index.html (SPA fallback)
  ├── /assets/*        → pakete gömülü dosyalar
  └── /api/*           → BACKEND (HTTP + WebSocket köprüsü)
```

Dikkat edilen iki nokta:

- **Port sabit** (18420, doluysa yukarı doğru taranır). Session cookie'si
  `127.0.0.1:<port>` origin'ine bağlı olduğu için her açılışta değişen bir port
  kullanıcıyı her seferinde çıkışa düşürürdü.
- **`Set-Cookie` yeniden yazılır.** Backend cookie'yi kendi alan adı için TLS
  üzerinden üretir, yani `Domain=` ve `Secure` ile gelir; loopback origin'i düz
  HTTP olduğu için webview böyle bir cookie'yi sessizce atardı. Bu iki attribute
  kaldırılır (`SameSite=None` da `Lax`'a indirilir). Cookie cihazın loopback
  arayüzünden hiç çıkmadığı için bu güvenli.

---

## Gereksinimler

| Araç | Sürüm | Not |
| --- | --- | --- |
| Rust | stable | `rustup` |
| Bun | 1.x | frontend build'i |
| JDK | 17 | Android Gradle |
| Android SDK | platform 35, build-tools 35 | `cmdline-tools` yeterli |
| Android NDK | 27.x | `NDK_HOME` |
| Xcode | 15+ | **yalnızca iOS**, macOS şart |

Ortam değişkenleri (bu makinede kurulu):

```
ANDROID_HOME=D:\Android
NDK_HOME=D:\Android\ndk\27.2.12479018
JAVA_HOME=C:\Program Files\Microsoft\jdk-17.0.20.101-hotspot
```

Rust hedefleri:

```sh
rustup target add aarch64-linux-android armv7-linux-androideabi i686-linux-android x86_64-linux-android
# iOS (macOS'ta):
rustup target add aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios
```

---

## Geliştirme

```sh
git submodule update --init --recursive
bun run frontend:install

# Backend'i host makinede çalıştır (127.0.0.1:8080), sonra cihaza yönlendir:
adb reverse tcp:8080 tcp:8080

bun run android:dev
```

`adb reverse` sayesinde emülatör/cihaz içindeki `127.0.0.1:8080`, geliştirme
makinendeki backend'e düşer — web'deki Vite proxy'sinin hedefiyle aynı adres.

Masaüstünde hızlı deneme için `bun run dev` (aynı proxy, aynı origin).

Dev modunda `/api` dışındaki istekler Vite dev sunucusuna proxy'lenir, yani HMR
de bu origin üzerinden çalışır.

## Sürüm derlemesi

```sh
HEZARFEN_BACKEND_ORIGIN=https://api.hezarfen.co bun run android:build
HEZARFEN_BACKEND_ORIGIN=https://api.hezarfen.co bun run ios:build   # macOS
```

`HEZARFEN_BACKEND_ORIGIN` derleme anında gömülür. İmzalı bir build'i yeniden
derlemeden staging'e çevirmek için (QA) cihazda
`<app-config>/backend.txt` dosyasına origin yazmak yeterli.

## Frontend'i güncelleme

Submodule belirli bir commit'i sabitler; kendiliğinden ilerlemez — mobil sürüm
test edilmemiş bir frontend commit'iyle bozulmasın diye böyle.

```sh
bun run sync-frontend   # frontend'i main'e taşır
git add frontend && git commit -m "frontend: bump to <sha>"
```

`.github/workflows/sync-frontend.yml` bunu haftalık olarak yapar: bump eder,
Android build'ini koşar, geçerse PR açar.

---

## iOS

iOS derlemesi macOS + Xcode gerektirir; Windows'ta mümkün değil. CI
(`.github/workflows/build.yml`) `macos-14` runner'ında `cargo tauri ios build`
koşar. App Store dağıtımı için `co.hezarfen.app` bundle identifier'ına ait bir
provisioning profile ve imzalama sertifikası gerekir.

Loopback sunucusu iOS'ta da çalışır; `Info.plist` içindeki
`NSAllowsLocalNetworking` istisnası ATS'in düz HTTP loopback trafiğini
engellememesi için gereklidir (`tauri ios init` sonrası eklenir).
