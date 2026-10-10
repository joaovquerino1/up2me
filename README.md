# up2me

**Português** · [English](#english)

Um app **nativo para macOS (Apple Silicon)**, escrito em **Rust**, para pesquisar e baixar vídeos do YouTube.
Interface em **português (Brasil)** e inglês. Usa [`yt-dlp`](https://github.com/yt-dlp/yt-dlp) e [`ffmpeg`](https://ffmpeg.org) por baixo dos panos — e **atualiza o `yt-dlp` sozinho**.

## Recursos

- **Busca no YouTube** dentro do app (ou cole um link).
- **Filtros e ordenação:** mais recentes, mais antigos, mais/menos visualizados, mais longos, mais curtos, melhor avaliados; data de envio, duração, mínimo de visualizações, HD, 4K, com legendas, Creative Commons e ocultar Shorts.
- **Formatos:** vídeo MP4 (H.264/AAC, até 4K), áudio MP3 (com capa) ou M4A.
- **Faixas de áudio:** escolha o idioma da dublagem quando o vídeo tiver várias.
- **Legendas:** baixe como `.srt`, embuta no MP4 ou baixe só as legendas (manuais e automáticas).
- **Fila de downloads** com progresso, velocidade, tempo restante e cancelamento.
- **Ferramentas embutidas** no `.app` (`yt-dlp`, `ffmpeg`, `ffprobe`), sem instalar nada.
- **Atualização automática do `yt-dlp`** (1x por dia, com verificação de checksum).
- Notificação do macOS ao concluir um download.

## Instalar

1. Baixe o `up2me-*-arm64.dmg` na página de [Releases](https://github.com/joaovquerino1/up2me/releases).
2. Abra o DMG e arraste o **up2me** para **Applications**.
3. **Primeira abertura:** o app é assinado apenas localmente (não é notarizado pela Apple), então o macOS vai bloqueá-lo. Tente abrir uma vez, depois vá em **Ajustes do Sistema → Privacidade e Segurança** e clique em **Abrir Mesmo Assim**. Ou, no Terminal:

```bash
xattr -dr com.apple.quarantine /Applications/up2me.app
```

## Requisitos

- Mac com Apple Silicon (M1 ou mais novo), macOS 12+.
- [Rust](https://www.rust-lang.org/tools/install) (`brew install rust`) para compilar.

## Compilar

```bash
git clone https://github.com/joaovquerino1/up2me.git
cd up2me
./scripts/bundle.sh        # baixa yt-dlp/ffmpeg, compila e gera target/up2me.app
open target/up2me.app
./scripts/make-dmg.sh    # (opcional) gera o instalador .dmg
```

- `UP2ME_NATIVE=1 ./scripts/bundle.sh` otimiza o binário para o chip da sua máquina (ex.: M3). O padrão roda em qualquer chip M.
- Para desenvolver sem embutir as ferramentas: `brew install yt-dlp ffmpeg` e `cargo run`.
- O app é assinado apenas localmente (ad-hoc). Ao abrir em outro Mac, use clique direito → **Abrir** na primeira vez.

## Publicar uma nova versão

1. Aumente `version` em `Cargo.toml` (ex.: `0.2.0`) e faça commit.
2. Crie e envie a tag: `git tag v0.2.0 && git push origin v0.2.0`.
3. O workflow **Release** compila, gera o DMG e publica o release no GitHub automaticamente. (Em *Actions → Release → Run workflow* dá para fazer um teste que só gera o DMG, sem publicar.)

## Aviso legal

Baixar conteúdo do YouTube pode violar os [Termos de Serviço](https://www.youtube.com/t/terms) da plataforma e direitos autorais. Use o up2me apenas com conteúdo que você tem direito de salvar (próprio, licenciado, domínio público, Creative Commons etc.). Você é responsável pelo uso que faz do software. Este projeto não é afiliado ao YouTube/Google.

## Licenças de terceiros

O código do up2me é [MIT](LICENSE). O script `scripts/fetch-tools.sh` baixa, no momento do build, ferramentas com licenças próprias, que **não** fazem parte deste repositório: `yt-dlp` (Unlicense) e `ffmpeg`/`ffprobe` (build estático sob GPL, de [ffmpeg.martin-riedl.de](https://ffmpeg.martin-riedl.de)). Se você distribuir o `.app` pronto, respeite as licenças desses binários.

---

## English

A **native macOS (Apple Silicon)** app written in **Rust** to search and download YouTube videos, with a **Brazilian Portuguese** and English interface. It drives [`yt-dlp`](https://github.com/yt-dlp/yt-dlp) and [`ffmpeg`](https://ffmpeg.org) and **keeps `yt-dlp` up to date automatically**.

### Features

- In-app **YouTube search** (or paste a link).
- **Filters & sorting:** newest, oldest, most/least viewed, longest, shortest, top rated; upload date, duration, minimum views, HD, 4K, subtitles, Creative Commons, hide Shorts.
- **Formats:** MP4 video (H.264/AAC, up to 4K), MP3 audio (with cover art) or M4A.
- **Audio tracks:** pick the dubbed language when a video has several.
- **Subtitles:** save as `.srt`, embed into the MP4, or download subtitles only (manual and auto-generated).
- **Download queue** with progress, speed, ETA and cancel.
- **Bundled tools** (`yt-dlp`, `ffmpeg`, `ffprobe`) — nothing to install.
- **Automatic `yt-dlp` updates** (once a day, checksum-verified).

### Install

1. Download `up2me-*-arm64.dmg` from [Releases](https://github.com/joaovquerino1/up2me/releases).
2. Open the DMG and drag **up2me** into **Applications**.
3. **First launch:** the app is only ad-hoc signed (not Apple-notarized), so macOS will block it. Try to open it once, then go to **System Settings → Privacy & Security** and click **Open Anyway**. Or in Terminal:

```bash
xattr -dr com.apple.quarantine /Applications/up2me.app
```

### Build

```bash
git clone https://github.com/joaovquerino1/up2me.git
cd up2me
./scripts/bundle.sh        # fetches yt-dlp/ffmpeg, builds, produces target/up2me.app
open target/up2me.app
```

`UP2ME_NATIVE=1 ./scripts/bundle.sh` tunes the binary for your own chip; the default runs on any M-series Mac. The app is ad-hoc signed, so on another Mac use right-click → **Open** the first time.

### Releasing

Bump `version` in `Cargo.toml`, commit, then `git tag v0.2.0 && git push origin v0.2.0`. The **Release** workflow builds the DMG and publishes the GitHub release. *Actions → Release → Run workflow* does a dry run that only builds the DMG.

### Legal

Downloading from YouTube may violate its Terms of Service and copyright law. Only download content you have the right to save. You are responsible for how you use this software. Not affiliated with YouTube/Google.

### Third-party licenses

up2me's code is [MIT](LICENSE). `scripts/fetch-tools.sh` downloads separately-licensed tools at build time (not part of this repo): `yt-dlp` (Unlicense) and `ffmpeg`/`ffprobe` (static GPL build from ffmpeg.martin-riedl.de). If you redistribute a built `.app`, honor those licenses.
