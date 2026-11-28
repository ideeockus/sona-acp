# Agent Control Plane

Control plane для агентов на базе Pi: идентичность, контекст, полномочия и выполнение задач — раздельно и под аудитом.

Каждый Run — Pi в своём Docker-контейнере. Каждый вызов инструмента проходит Gateway, каждое действие пишется в Journal. Оператор работает через веб-UI или `sapctl`. Как устроено — [docs/spec.md](docs/spec.md), почему так — [docs/decisions.md](docs/decisions.md), слабые места — там же, «Известные дыры».

## Что нужно

- Rust (stable), Docker, Node 22.
- Ключ провайдера модели, который знает Pi: например, `OPENCODE_API_KEY` для `opencode-go` или `ANTHROPIC_API_KEY` для `anthropic`. Свой провайдер описывается в `models.json` (`models_json` в конфиге).

## Локально

Образ агента и бинари:

```text
docker build -t sona-plane-agent:dev images/agent
cargo build --release -p control-plane -p control-plane-cli
```

Конфиг — по образцу [control-plane.example.toml](control-plane.example.toml), с абсолютными путями. На macOS (OrbStack, Docker Desktop):

```toml
operator = "user"
data_dir = "/path/to/sona-data"
agent_image = "sona-plane-agent:dev"
extension = "/path/to/control-plane/extension/src"
listen = "127.0.0.1:7700"
api_url = "http://host.docker.internal:7700"
provider_env = ["OPENCODE_API_KEY"]
```

В конфиге только имена переменных. Сами ключи control plane берёт из своего окружения и передаёт агенту.

`init` создаёт базы и один раз печатает credential оператора. Сохрани его: второй раз его не показать.

```text
target/release/control-plane --config control-plane.toml init
OPENCODE_API_KEY=… target/release/control-plane --config control-plane.toml serve
```

UI во втором терминале, на http://localhost:5173. Dev-сервер Vite проксирует `/v1` на `127.0.0.1:7700`, другой адрес задаёт `SONA_API`. Вход — по credential.

```text
npm --prefix ui ci
npm --prefix ui run dev
```

Первый агент — в UI (Agents → New agent, провайдер и модель выбираются из списка) или через `sapctl`:

```text
export SAPCTL_TOKEN=…
target/release/sapctl agents create coder.json
target/release/sapctl send coder "Привет"
target/release/sapctl status
```

```json
{
  "alias": "coder",
  "title": "Writes code",
  "model": { "provider": "opencode-go", "id": "glm-5.3" },
  "access": ["read_files", "write_files", "send_messages"],
  "allow_message_to": ["user"],
  "prompt": "You are @coder."
}
```

Модель — из `sapctl models`: там те, для которых у Pi есть ключ. Карточку с другой моделью control plane не сохранит.

`access` — группы инструментов: `read_files`, `write_files`, `shell`, `tasks`, `send_messages`. `shell` даёт `bash` без проверок Gateway: внутри контейнера агент делает что угодно (G4).

Предохранители (D100) по умолчанию: 30 Run и $5 на цепочку, 20 пробуждений агента за 10 минут. Настраиваются в `[limits]`. Стоимость берётся из отчёта Pi; если Pi не знает цену модели, Run стоит $0 и денежный лимит не сработает.

## На VPS

1. Сеть для агентов, один раз (D88):

   ```text
   docker network create --subnet 172.30.0.0/24 \
     -o com.docker.network.bridge.enable_icc=false sona-agents
   ```

2. Конфиг — как в [control-plane.example.toml](control-plane.example.toml): API слушает шлюз этой сети, `172.30.0.1:7700`, и наружу не смотрит.
3. `control-plane init`, затем `control-plane serve` под systemd или другим супервизором, ключи провайдера — в его окружении.
4. UI: `npm --prefix ui ci && npm --prefix ui run build`, содержимое `ui/dist` — в каталог, который раздаёт прокси.
5. TLS, статика и `/v1` — reverse proxy на поддомене `dash.`, пример — [deploy/Caddyfile](deploy/Caddyfile). Cookie сессии ходит только по HTTPS, поэтому без прокси UI с другой машины не работает.

Firewall хоста пока на операторе (G5): агенту нужен только API, но сейчас у него открыт исходящий интернет (G3).

## Тесты

```text
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
npm --prefix ui run check
```

e2e гоняет настоящий бинарь и настоящий Pi в Docker, модель — заглушка, токены не тратятся. Нужен образ агента:

```text
cargo test -p control-plane --test e2e -- --ignored
```

На Linux e2e идёт через сеть агентов; переменные — в начале [crates/control-plane/tests/e2e.rs](crates/control-plane/tests/e2e.rs).
