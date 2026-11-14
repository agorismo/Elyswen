# Elyswen

Launcher de Minecraft: Java Edition minimalista, em Rust.

- Login oficial (Microsoft → Xbox → Minecraft), com renovação automática do token
- Todas as versões da Mojang: releases, snapshots, beta e alpha
- Baixa client, libraries, assets, natives **e o Java certo** para cada versão (nada para instalar)
- Discord Rich Presence ("Jogando Minecraft 1.21.4" + tempo de jogo)
- Instâncias: cada uma com versão, loader e pasta própria (saves, config, mods)
- Fabric: instale como loader em uma instância nova
- Mods: busca e instalação pelo Modrinth, com dependências obrigatórias
- Estatísticas: tempo jogado por instância e histórico de sessões
- Crash: quando o jogo fecha com erro, mostra a causa provável (RAM, Java, mod incompatível)
- Refresh token guardado no cofre do sistema (Credential Manager no Windows)
- Janela única, sem frescura. ~1 arquivo por responsabilidade em `src/`

## Compilar (Windows)

```
rustup default stable
cargo run --release
```

O executável fica em `target/release/elyswen.exe`. Para publicar uma versão no GitHub: `git tag v0.2.0 && git push origin v0.2.0` (o workflow `release.yml` cria a release com o `.exe`). Também tem um workflow em
`.github/workflows/build.yml` que gera os binários de Windows/Linux/macOS no GitHub Actions.

CLI útil para testar: `elyswen list` (lista versões) e `elyswen install 1.21.4` (baixa tudo sem abrir o jogo).

## Configuração (uma vez)

### 1. Client ID da Microsoft (obrigatório para logar)

1. Entre em <https://portal.azure.com> → **App registrations** → **New registration**
2. Tipo de conta: **Personal Microsoft accounts only**
3. Em **Authentication**, ative **Allow public client flows** = Sim
4. Copie o **Application (client) ID** e cole em ⚙ → *Client ID (Azure)*

> A Mojang exige aprovação de apps novos para usar a API do Minecraft. Se o login terminar com
> `403 ... Invalid app registration`, peça a liberação em <https://aka.ms/AppRegInfo>.
> Não use o client ID de outro launcher.

### 2. Discord Rich Presence (opcional)

1. <https://discord.com/developers/applications> → **New Application** (o nome aparece como "Jogando ...")
2. Copie o **Application ID** e cole em ⚙ → *Discord App ID*
3. Em **Rich Presence → Art Assets**, envie uma imagem com a chave `minecraft`

Também dá para usar as variáveis `ELYSWEN_CLIENT_ID` e `ELYSWEN_DISCORD_APP_ID`.

## Onde ficam os arquivos

Tudo em `%APPDATA%\elyswen\data` (Windows) / `~/.local/share/elyswen` (Linux): `versions`, `libraries`,
`assets`, `runtime` (Java), `instances/<id>` (saves, config e mods de cada instância), `logs/latest-launch.log`.
O login fica em `account.json`; o refresh token fica no cofre do sistema quando ele funciona.

## Limitações atuais

- Loaders: vanilla e Fabric. Quilt, NeoForge e Forge ainda não
- Mods só pelo Modrinth (sem CurseForge nem modpacks `.mrpack` ainda)
- Sem skins, bandeja, tradução e troca automática do launcher (só avisa que há versão nova)
- Versões muito antigas (LWJGL 2) não têm natives para Mac ARM / Linux ARM
- Uma conta por vez
