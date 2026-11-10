# keymap — cliamp's transport, for people coming from cliamp (issue #38)

velo's transport keys relearned themselves: `,` `.` `<` `>` seek and `n`/`p`
skip, which is backwards for anyone who arrives from cliamp. The keymap moves
to cliamp's habits — the author's words: "tratar de mantener los keymaps de
cliamp para que sea mas facil usar la app para usuarios que vengan de ahi".

## Specs

**S1 — Every binding works where the feature lives.** The transport (`>`.
`.`, `n` next; `<`, `,` previous) answers everywhere. `A` adds and plays from
that entry and `x` removes, wherever a selection can be queued or removed.
The folder prompt opens from any screen that has it today (`o`, `ctrl+l`).
`L` lands on the saved playlists source and `p` on the queue view. Paging
(`pgdn`/`pgup`, `ctrl+d`/`ctrl+u`) and `home`/`end` work in every list that
scrolls — the library's lists, the queue, the search results, the sources
screen and the radio's lists.

**S2 — No key is bound twice.** Where a new key collides with an overlay
that owns its keys (settings, help, pickers, prompts), the overlay keeps
owning them: the new globals never fire while one is open, exactly as
today. The radio keeps `o` (its order card); the sources screen keeps the
library's editing keys as no-ops.

**S3 — The help card and the README tell the truth.** The transport rows,
`A`, `o`/`ctrl+l`, `ctrl+p`, `L`, `p`, `z`, `x`, `m`, the paging rows and
the aliases are all written as the code does them. A row that no longer
exists (`p` = previous, `s` = shuffle, `d` = remove, `M` = mono, the
`,` `.` seek pair) is rewritten or removed, never left. The footer hints and
the settings card follow the same truth.

**S4 — Modes that cannot act say so.** `x` on a row that cannot be removed
and `A` on a row that cannot be queued answer with the existing refusal
style: one status line, nothing else moved.

## Tasks

| ID | Spec | Route | Task |
|:--|:--|:--|:--|
| T1 | S1, S3 | tests + `src/ui/mod.rs` | The transport: `>`/`.`/`n` next, `<`/`,` previous, the `,` `.` `<` `>` seek pair removed; help rows, footer hints, README |
| T2 | S1, S3 | tests + `src/ui/mod.rs` | `A` adds and returns `PlayQueue` from the first added entry; `x` removes in the queue and deletes a playlist name; `z` shuffle, `m` mono, `s`/`d`/`M` unbound |
| T3 | S1 | tests + `src/ui/mod.rs` | `o`/`ctrl+l` open the library folder prompt, `ctrl+p` opens settings, `L` opens the playlists source, `p` opens the queue view |
| T4 | S1, S2 | tests + `src/ui/mod.rs` + `src/ui/radio.rs` | `ctrl+c` quit, `ctrl+k` help, `ctrl+j` the seek prompt, `home`/`end`, `pgdn`/`pgup` and `ctrl+d`/`ctrl+u` paging, `shift+↑`/`shift+↓` moving an entry — in the keymap, the search, the help and settings cards, the sources screen and the radio |
| T5 | S4 | tests + `src/ui/mod.rs` | `x` where nothing can be removed and `A` where nothing can be queued say so in one status line |

## Log

**L1** — the author, on the issue: "tratar de mantener los keymaps de cliamp
para que sea mas facil usar la app para usuarios que vengan de ahi".
