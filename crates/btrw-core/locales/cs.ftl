app-title = Btrfs Rewind
app-subtitle = Porovnání a obnova Btrfs snapshotů

timeline-header = Časová osa
timeline-empty = Nenalezeny žádné snapshoty — použij --left / --right nebo spusť přes sudo
source-live = ŽIVÝ
panel-left-tag = L
panel-right-tag = P

snapshot-count = { $count ->
    [one] { $count } snapshot
    [few] { $count } snapshoty
   *[other] { $count } snapshotů
}

key-f1 = F1 Nápověda
key-f2 = F2 Diff
key-f3 = F3 Historie
key-f4 = F4 ExtDiff
key-f5 = F5 Obnovit
key-f6 = F6 UložitJako
key-f7 = F7 Filtr
key-f8 = F8 Subvol
key-f9 = F9 Nastavení
key-f10 = F10 Konec
key-src = 1/2 Zdroj
key-hide = h Jen změny
key-swap = ^U Prohodit

status-ready = Připraveno — Tab panel, [ ] osa, F8 subvol, F9 nastavení, F10 konec
status-filter = Filtr: { $query }
status-hidden-unchanged = [jen změny]
status-errors = { $count } chyb (stiskni i)
status-sort = Řazení: { $label }

sort-name-asc = jméno ↑
sort-name-desc = jméno ↓
sort-size-desc = velikost ↓
sort-size-asc = velikost ↑
sort-delta-desc = změna ↓
sort-delta-asc = změna ↑
sort-mtime-desc = datum ↓
sort-mtime-asc = datum ↑

config-title = Nastavení (F9) — ukládá se automaticky
config-hint = ↑↓ výběr  Enter/Space/←→ změna  Esc zavřít
config-saved = Konfigurace uložena.
config-save-failed = Uložení selhalo: { $detail }
cfg-language = Jazyk
cfg-theme = Barevné téma
cfg-hide-unchanged = Skrývat shodné soubory
cfg-show-size = Sloupec: Velikost
cfg-show-delta = Sloupec: Změna velikosti
cfg-show-mtime = Sloupec: Datum změny (mtime)
cfg-show-permissions = Sloupec: Práva
perm-off = vypnuto
perm-octal = oktalově (0644)
perm-octal-owner = oktalově + vlastník:skupina
cfg-mouse = Ovládání myší (klik / dvojklik jako MC)
cfg-show-metadata = Zobrazovat čistě metadatové změny
cfg-create-backup = Obnova: vytvářet zálohu .bak
cfg-live-strategy = Strategie pro živý stav
val-on = ZAP
val-off = VYP

info-title = Chyby a vynechané mountpointy (i)
info-hint = ↑↓ posun  Esc/q zavřít
info-errors-hdr = Chyby ({ $count }):
info-excluded-hdr = Vynechané mountpointy / vnořené subvolumy ({ $count }):
info-none = (žádné)

subvol-picker-title = Vyber subvolume (F8 / g)
subvol-picker-hint = ↑↓ výběr  Enter potvrdit  Esc zpět
subvol-picker-hint-initial = ↑↓ výběr  Enter potvrdit  Esc/q konec

picker-title-left = Vyber levý zdroj (Alt+F1 / 1) | Celkem / Exclusive
picker-title-right = Vyber pravý zdroj (Alt+F2 / 2) | Celkem / Exclusive
picker-hint = ↑↓ výběr  ←→/Enter rozbalit/potvrdit  Space/s velikosti  Esc zpět
picker-du-running = Počítám velikosti { $done }/{ $total }… (Esc zruší)
picker-unknown-date = Snapshoty bez data

diff-title = Diff: { $path }
diff-hint = ↑↓ PgUp/PgDn posun  ←→ vodorovně  v unified/vedle sebe  Esc/q zpět
diff-identical = Obsah souborů je shodný.
diff-binary = Binární soubor — textový diff není dostupný.
diff-too-large = Soubor přesahuje velikostní limit pro textový diff.
diff-too-many-lines = Soubor přesahuje limit počtu řádků pro textový diff.
diff-non-regular = Speciální soubor, adresář nebo symlink — textový diff není dostupný.
diff-error = Soubor nelze přečíst: { $detail }

diff-menu-title = Rozsah diffu a export (F2)
diff-menu-dir = Aktuální adresář — plochý seznam změn
diff-menu-snapshot = Celý snapshot — plochý seznam změn
diff-menu-export = Exportovat .patch pro aktuální pohled / soubor
diff-menu-tree = Návrat do adresářového stromu MC
diff-menu-hint = ↑↓ výběr  Enter potvrdit  Esc zpět

export-title = Exportovat unified diff (.patch)
export-scope-label = Rozsah:
export-scope-selected = Vybraný objekt ({ $name })
export-scope-dir = Aktuální adresář ({ $dir })
export-scope-snapshot = Celý snapshot
export-hint = Tab rozsah/adresář/název  ←→ změna/kurzor  Enter otevřít či uložit  Esc zpět
export-ok = Exportováno { $exported } souborů do { $path } (přeskočeno: { $skipped })
export-err = Export patche selhal: { $detail }

extdiff-not-found = Nenalezen externí diff nástroj (nastav v F9 nebo $DIFFPROG)
extdiff-need-file = Pro externí diff (F4) vyber běžný soubor na obou stranách
extdiff-too-large = Soubor přesahuje limit pro externí diff ({ $limit })
extdiff-failed = Externí diff selhal: { $detail }

cfg-diff-format = Výchozí zobrazení diffu
cfg-ext-diff = Externí diff příkaz (F4)

flat-scanning = Skenuji změny… { $scanned } položek ({ $path }) — Esc zruší
flat-mode-badge = [PLOCHÝ: { $scope }]
dir-chg-short = { $count } změn

tt-title = Časová osa souboru: { $path } ({ $versions } verzí ze { $total } snapshotů)
tt-loading = Hledám změny { $path } napříč { $total } snapshoty… (Esc zruší)
tt-empty = Pro tento soubor nebyly ve snapshotech nalezeny žádné verze.
tt-hint = ←→ verze  Space označit (2=diff)  F2 export  F5/F6 obnova  n/p změna  m ({ $mode })  Esc zpět
tt-need-two-pinned = Pro export diffu (F2) nejprve označ 2 verze klávesou Space.
tt-already-live = Aktivní verze je ŽIVÝ stav — vyber šipkami ←→ snapshot k obnově (F5).
tt-pinned-header = Diff označených verzí: { $from } → { $to }
tt-mode-prev = vs předchozí
tt-mode-live = vs ŽIVÝ
tt-mode-none = čistý text

restore-title = POTVRZENÍ OBNOVY SOUBORU (ZÁPISOVÁ OPERACE)
restore-saveas-title = Uložit soubor ze snapshotu jinam (F6)
restore-saveas-dir = Adresář: { $dir }
restore-saveas-name = Název souboru:
restore-saveas-target = Cíl: { $path }
restore-saveas-hint = Tab adresář/název  ←→ kurzor/navigace  Enter otevřít či potvrdit  Esc zpět
restore-confirm-hint = y POTVRDIT ZÁPIS   d náhled diffu   n / Esc zrušit
restore-warn-overwrite = POZOR: Cílový soubor EXISTUJE a bude PŘEPSÁN!
restore-warn-hardlink = POZN: Cíl má { $nlink } pevných odkazů — atomická obnova nahradí jen tuto cestu novým inodem.
restore-warn-new = Cílový soubor neexistuje — bude vytvořen nový soubor.
restore-label-source = Zdrojový snapshot:
restore-label-dest = Cíl zápisu:
restore-label-src-meta = Zdrojový soubor:
restore-label-dst-meta = Aktuální živý:
restore-label-options = Nastavení:
restore-opt-backup-on = záloha .bak ZAP
restore-opt-backup-off = záloha .bak VYP
restore-opt-reflink-auto = Btrfs reflink (FICLONE) AUTO
restore-opt-reflink-never = reflink VYP
restore-need-file = Pro obnovu (F5/F6) vyber běžný soubor ze snapshotu.
restore-need-live = F5 vyžaduje cílový Btrfs subvolume; pro vlastní adresáře použij F6 (UložitJako).
restore-ok = Obnoveno do { $dest } ({ $info })
restore-err = Obnova selhala: { $detail }
restore-err-no-filename = cílová cesta nemá název souboru
restore-err-dest-dir = cíl { $path } je adresář (přepsání odmítnuto)
restore-err-dest-symlink = cíl { $path } je symlink (přepsání odmítnuto)
restore-err-dest-not-regular = cíl { $path } není běžný soubor
restore-err-dest-inspect = nelze ověřit cíl { $path }: { $reason }
restore-err-perm-denied = přístup odepřen
restore-err-inside-snapshots = zápis do adresáře snapshotů je zakázán
restore-err-ro-subvol = zápis do Btrfs subvolumu jen pro čtení je zakázán: { $path }
restore-err-ro-fs = cílový souborový systém je připojen jen pro čtení: { $path }
restore-err-dest-symlink-generic = přepsání symlinku v cíli je zakázáno
restore-err-dest-dir-generic = přepsání adresáře v cíli je zakázáno
restore-err-dest-special-generic = přepsání speciálního souboru v cíli je zakázáno
restore-err-dest-changed = cílový soubor se od potvrzení změnil; potvrď prosím znovu
restore-err-src-changed = zdrojový soubor se od potvrzení změnil; zkus to prosím znovu
restore-err-mount-boundary = živá obnova F5 přes hranici přípojného bodu nebo subvolumu je zakázána

help-title = Klávesové zkratky (styl MC)
help-body =
    Tab              Přepnout aktivní panel (Levý / Pravý)
    ↑ ↓ PgUp PgDn    Pohyb v seznamu (synchronizovaně)
    Enter            Vstup do adresáře nebo diff souboru
    Space            Async rekurzivní souhrn adresáře (změny + velikosti, 4 Hz)
    F2 nebo f        Plochý seznam změn (adresář / celý snapshot / strom)
    Backspace        O úroveň výš (nadřazený adresář)
    1 / Alt+F1       Výběr zdroje levého panelu (snapshot / ŽIVÝ)
    2 / Alt+F2       Výběr zdroje pravého panelu (snapshot / ŽIVÝ)
    F8 nebo g        Přepnout subvolume (oba panely na ŽIVÝ stav)
    Ctrl+←/→ či [ ]  Posun aktivního panelu po časové ose
    Shift+[ / ]      Skok aktivního panelu po dnech na časové ose
    Ctrl+U           Prohození levého a pravého panelu
    F3               Zobrazit diff vybraného souboru
    F7 nebo /        Filtrování podle názvu (Esc smaže)
    h                Přepnout skrývání shodných souborů
    n / s / c / t    Řazení: jméno / velikost / změna / datum (opakování obrací)
    i                Zobrazit chyby a vynechané mountpointy
    F9               Otevřít nastavení (ukládá se do config.toml)
    Myš              Klik = výběr, dvojklik = otevřít/diff, kolečko = posun
    r                Znovu načíst aktuální adresář
    F10 nebo q       Konec

    Stiskni Esc nebo Enter pro zavření nápovědy.

err-config-read = Nepodařilo se načíst konfigurační soubor: { $detail }
err-config-write = Nepodařilo se uložit konfigurační soubor: { $detail }
err-config-parse = Neplatná syntaxe konfigurace: { $detail }
status-loading-dir = Načítání adresáře… (Esc zrušit)
status-exporting-patch = Export patche… (Esc zrušit)
status-worker-error = Úloha na pozadí byla neočekávaně ukončena.
