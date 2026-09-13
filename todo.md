# TODO — доработки ztop

## Инфраструктура релиза

- [x] 1. **CI (GitHub Actions)** — `cargo build`, `cargo test`, `cargo clippy`, `cargo fmt --check` на push/PR. → `.github/workflows/ci.yml`
- [x] 2. **LICENSE** — добавить файл MIT (в Cargo.toml уже указано `license = "MIT"`). → `LICENSE`
- [x] 3. **Версия** — Cargo.toml → 0.7.0, release-workflow → `.github/workflows/release.yml` (сборка бинарников linux/macos x86_64+aarch64 по тегу `v*`). Осталось: закоммитить и запушить тег `v0.7.0`.

## Код

- [x] 4. **Clippy** — все предупреждения закрыты, в CI включён `-D warnings`.
- [x] 5. **Разбить `ui.rs`** → `src/ui/`: mod (draw), chrome (header/tabs/diagnoses/footer), по модулю на таб, modal, util. Крупнейший файл теперь 397 строк.
- [x] 6. **Хвосты review-report**:
  - `record.rs:122` — `CollectorMsg::Reset` теперь `unreachable!()` вместо фиктивного `LogStreamStatus`;
  - `ssh.rs:36` — комментарий исправлен на `~/.ssh`.

## Функциональность (Post-MVP из roadmap)

- [ ] 7. **diaginfo → структурированные блоки** — запуск `zabbix_server -R diaginfo`, парсинг historycache / valuecache / preprocessing, отдельная панель.
- [ ] 8. **Queue по prefer-зонам** — отдельный график.
- [ ] 9. **Компакция записей** — gzip на лету для `--record` (сейчас 36–180 МБ/час инцидента, ожидаемо ~10× сжатие).
- [ ] 10. **Интеграционный smoke-тест в CI** — поднять docker-стенд, запустить ztop headless на пару тиков, проверить что все 4 источника отдали данные.

## Мелочи

- [x] 11. **`review-report.md`** — перенесён в `docs/review-report.md`.
- [ ] 12. **rustls вместо openssl** — довести до конца закомментированный в Cargo.toml рецепт (статические бинарники без системного openssl).
