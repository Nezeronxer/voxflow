# Таск 01 - условия ревью, не выполнены (прогон остановлен 2026-10-11)

База: ветка `autopilot/t01-live-typing`, коммит f21dc51 (397 тестов зелёные, но не принят).

1. [блокирует] Esc на фразе N отменяет только поколение N; более ранние финалы в полёте вставляются. Сейчас Esc сносит все (engine.rs ~3123-3131, тест `escape_cancels_every_final_in_flight...`).
2. [блокирует] Исход «финал упал» не зависит от гонки между `drop(ticket)` и `retire_superseded_draft_token` (~3004-3013 vs 3076-3085): Start не стирает черновик упавшего финала.
3. [блокирует] Удаление любого `gate.open()` (local_partial_loop, live_insert_always, live_insert_auto_committed) краснит тест.
4. Тест на режим never: финал вытесненного Start поколения вставляется.
5. Лог в ветке `!final_generation_is_current` (~4211) называет отмену Esc.
6. `as_chunks` в speech_track - строка в коммите: для clippy новее 1.96, MSRV не поднимается.
