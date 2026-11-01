# DADABADA architecture

How DADABADA works inside: how records are stored, how a query becomes key ranges, how writes, access,
background work and administration fit together. How to *use* it (the API, the shells, the web admin,
operations) is in the documentation site; this file is for working on the code.

DADABADA is a native Linux database server written in Rust: one binary, run in a terminal or as a service,
answering one port. It keeps records with collections, any number of indexes per record, tags, references,
unique ids, access levels, private grants and subscriptions. The user who saves a record decides its
indexing, access level and reference rules. There is no schema.

One running process is one database, in one data folder. The operating system user who owns that folder
is the admin: there is no admin account on the network.

## Contents

1. [Storage](#storage)
2. [Key encoding](#key-encoding)
3. [Query views](#query-views)
4. [Writes](#writes)
5. [Access rules](#access-rules)
6. [Background worker](#background-worker)
7. [Users and network](#users-and-network)
8. [Administration](#administration)
9. [Client-side encryption](#client-side-encryption)
10. [Wire API](#wire-api)
11. [Commands, settings and export](#commands-settings-and-export)
12. [Code layout](#code-layout)
13. [Build, release and tests](#build-release-and-tests)
14. [Scale](#scale)
15. [Design choices](#design-choices)

---

## Storage

DADABADA stores everything in **RocksDB**, through the `rocksdb` crate, statically linked, so the program
stays one binary.

The design rests on one sorted key space: every way of finding a record is a set of extra keys in a
byte-sorted store. RocksDB fits that well:

- **Atomic write batches across column families.** A record and all of its query keys land together or not
  at all, so a query never sees half a record and never misses a record that was just saved.
- **Forward and reverse iterators with bounded ranges** give ascending and descending order and starts-with.
- **Prefix compression inside data blocks.** Keys share long prefixes (collection, level, index name), and
  RocksDB stores each key as shared bytes plus a suffix, so the many keys behind each record cost far less
  disk than their raw length.
- **LZ4 per block (Zstd at the bottom level), a shared block cache, a bloom filter per table, checkpoints**
  for online backup, and a write-ahead log for crash safety.
- **BlobDB for large records.** Record data has no size cap, so the `rec` column family uses RocksDB's
  blob files: a value of `rocksdb.blob_min_kb` (default 4 KB) or more is written once to a blob file, and
  only a pointer goes into the sorted files. Compaction then moves pointers, not megabytes of JSON. View
  keys are never large, so only `rec` uses this.

Writes are not flushed to disk one by one: a saved write survives a crash of the process (the write-ahead
log is replayed on start), but the last writes before a power loss or an operating system crash can be
missing.

`store.rs` wraps RocksDB in a `Store` (get, exists, scan, write) and a `Batch` (puts, deletes and outbox
events that commit together). Column families:

| CF | key | value |
|---|---|---|
| `rec` | record_id | record body (MessagePack metadata + the raw JSON data bytes) |
| `ix` | query view keys ([Query views](#query-views)) | empty |
| `unq` | unique_id | record_id |
| `usr` | `u`+user_id / `n`+username, and saved sessions | user row / user_id |
| `key` | `h`+sha256(agent key) / `u`+user_id+`\0`+key_id | agent key row / empty |
| `acc` | `r`+record+`\0`+user and `u`+user+`\0`+record | grant row (granted by, can regrant) |
| `sub` | `t`+target+`\0`+subscriber and `s`+subscriber+`\0`+target | options, blocked |
| `feed` | subscriber+`\0`+record_id | empty |
| `ntf` | subscriber+`\0`+seq (8 bytes, big-endian) | notification JSON + created time |
| `cat` | catalog: collections, levels, tags, indexes, unique-id listing, with reversed and order keys | counters |
| `evt` | outbox sequence number (8 bytes, big-endian) | event |
| `meta` | settings, server secret, format and loose-rules versions | |

**The outbox.** Every batch that changes a record also adds its events to `evt`, under sequence numbers
taken when the batch is built. A batch can commit after a later one, so the outbox tracks which numbers are
still in flight, and the background worker never reads past the lowest one not yet committed. No event is
skipped, however the batches interleave.

---

## Key encoding

Every query reads ranges of sorted bytes. For that, every value is encoded so that byte order is the order a
person expects.

**Separator.** Key components are joined with `0x00`. Names, tags, collections and string values are
refused when they contain control characters, so `0x00` never appears inside a component. No escaping is
needed, and a shorter string sorts before a longer one that starts with it (`"ab"` < `"abc"`).

**Unicode normalization (NFC).** The same visible text can arrive as different bytes: `é` as one code point
(NFC) or as `e` plus a combining accent (NFD, common from macOS). Every collection name, tag, index name,
string index value, unique_id and username is converted to NFC when it is saved and again when it is read
from a query. The bytes then always match. Record `data` is never changed.

**Range end.** Valid UTF-8 never contains the byte `0xFF`, so the exclusive end of "everything starting
with P" is `P` + `0xFF`: one byte that can never collide with real data.

**Typed values.** A type byte comes first, so all values of one type sit together:

| type | byte | encoding | order |
|---|---|---|---|
| boolean | `0x01` | `0x00` false / `0x01` true | false < true |
| number | `0x02` | f64 as 8 bytes big-endian; positive values have the sign bit flipped, negative values have all bits flipped | numeric order, negative decimals included |
| string | `0x03` | UTF-8 bytes as given | code point order (UTF-8 byte order is code point order) |

Integers stay exact up to ±2^53.

**Reversed keys for ends-with.** For an index whose value is a string, or whose name contains `.`, a second
key goes into a reversed view:
- the name is reversed **per `.` segment**: `band.house.title` becomes `dnab.esuoh.eltit`
- a string value is reversed **by Unicode character, not by byte**, so it stays valid UTF-8: `report.pdf`
  becomes `fdp.troper`

"Ends with `.pdf`" then becomes "starts with `fdp.`" in the reversed view: one prefix scan.

### Loose match

Exact match compares bytes. Loose match compares a *loosened* form of the text, so people and agents find a
value without typing it exactly. For every string index value, the loose form is written as an extra key,
plus its reverse for ends-with. A query with `"match": "loose"` loosens its own value the same way and scans
those keys. Only built-in Unicode rules are used, no dictionaries.

The loose form is made in this order, for stored values and query values alike (`codec/loose.rs`):

1. **Compatibility forms (NFKC).** Full-width `ＡＢＣ１２` becomes `ABC12`; half-width `ｶﾀｶﾅ` becomes
   `カタカナ`; ligatures (`ﬁ` → `fi`), circled and superscript digits (`①` `²` → `1` `2`), Roman numeral
   characters (`Ⅳ` → `IV`) and Arabic presentation forms become their plain letters.
2. **Case folding (Unicode full folding).** `Desk LAMP` → `desk lamp`, `STRASSE` and `straße` → `strasse`,
   Greek final `ς` → `σ`, and every other cased script: Cyrillic, Armenian, Georgian, Cherokee and others.
3. **Accents are removed, only where they are accents.** Combining marks are dropped after letters in
   Latin, Greek, Cyrillic, Armenian, Hebrew, Arabic and Syriac:
   - `cafe` finds `café`, `Viet` finds `Việt`, `angstrom` finds `Ångström`, `αθηνα` finds `Αθήνα`,
     `елка` finds `ёлка`
   - Hebrew vowel points and Arabic vowel marks are ignored
   - In Indic scripts, Thai, Lao, Khmer, Myanmar and Tibetan, marks are vowels that change the word, so they
     are **kept**. So is the Japanese voicing mark (`が` ≠ `か`).
4. **Letters without a decomposition are mapped by a fixed table:** `ø`→`o`, `đ`→`d`, `ł`→`l`, `ħ`→`h`,
   `ŧ`→`t`, `æ`→`ae`, `œ`→`oe`, `þ`→`th`, `ð`→`d`, Turkish `ı`→`i` (with steps 2 and 3, `ISTANBUL` finds
   `İstanbul`).
5. **Script letter forms.**
   - Japanese: katakana → hiragana (`カタカナ` = `かたかな`), small kana → full size (`ゃ` = `や`).
   - Arabic, Persian, Urdu: `أ إ آ ٱ` → `ا`, `ى` and Persian `ی` → `ي`, Persian `ک` → `ك`, `ة` → `ه`,
     `ؤ` → `و`, `ئ` → `ي`, and the stretching mark `ـ` is removed.
   - Hebrew: final letters become their normal forms (`ך ם ן ף ץ` → `כ מ נ פ צ`).
6. **Digits from every script become ASCII digits:** `٢٠٢٦`, `۲۰۲۶`, `२०२६`, `๒๐๒๖` and `２０２６` all
   become `2026`.
7. **Korean is split into the letters that are typed.** `안녕` → `ㅇㅏㄴㄴㅕㅇ`; compound vowels and
   finals split too (`ㅘ` → `ㅗㅏ`, `ㄳ` → `ㄱㅅ`). Double consonants (`ㄲ ㄸ ㅃ ㅆ ㅉ`) stay whole, as they
   are one key on the keyboard. A word typed halfway is then a plain prefix: `안녀` and `안ㄴ` find `안녕`.
8. **Spaces, punctuation and invisible characters are removed:** every Unicode space, tabs and line breaks,
   every punctuation character (`- _ . , ' " / ( ) ! ? : ;` and their other-script forms), zero-width
   characters, variation selectors and soft hyphens. `newyork` finds `New York`, `email` finds `e-mail`,
   `usa` finds `U.S.A.`. Symbols and emoji (`★`, `$`, `😀`) are **kept**.
9. **NFC** at the end, so the bytes are stable.

**Korean initials.** For a string that contains Hangul syllables, one more key holds only each syllable's
first consonant, after steps 1–8: `안녕 하세요` → `ㅇㄴㅎㅅㅇ`, plus its reverse. When a loose query contains
Hangul consonants and no vowels or whole syllables (`ㅇㄴ`), the planner scans the initials key instead.

Rules:
- Every condition works with loose match: `=`, `>`, `<`, `>=` (starts with), `<=` (ends with) and `range`.
  Results come back in loose order.
- A query that is empty after loosening (only punctuation, such as `--`) is refused; it would match
  everything. A stored value that loosens to nothing gets no loose keys.
- Exact match stays the default (`"match": "exact"`).
- Index names, collection names and tags are always exact; only string index values have a loose form.
- **Not covered:** anything needing a dictionary (Chinese pinyin, Japanese kanji readings, simplified ↔
  traditional Chinese; an uploader can store a reading as its own index), and *contains* (sorted keys match
  the beginning, and the end through reversed keys).
- **Rule versions.** The loose rules are versioned (`loose_version` in `meta`). If an upgrade changes them,
  `start` says so, and `ddbd admin rebuild-loose` rewrites the loose and initials keys. Exact keys are
  never affected.

The catalog uses the same approach: collection names, tag names, index names and unique ids each have a
reversed catalog key, so every listing supports ends-with.

### Record ids

A record id has 16 base62 characters:
- 7 for the upload time in milliseconds (fixed width, so string order is time order)
- 5 random
- 4 derived from the uploader's user id (a cheap ownership pre-check)

`$uploaded` therefore needs no key of its own: record-id order *is* upload order. Two records one user saves
in the same millisecond can draw the same id; the new record then draws again under the record lock, up to
8 more times ([Writes](#writes)). An imported record keeps its id, and a duplicate import is refused.

**Counters inside keys.** `$updated` and `$referenced_count` are 8-byte big-endian u64 values, so numeric
order is byte order.

---

## Query views

A view key is (`views.rs`):

```
<scope> <sort> <record_id>
```

**Scopes** (who or what the listing is for):

| scope | holds |
|---|---|
| collection | every record of a collection at one access level |
| owner | one user's records in a collection, per level |
| reference | the records that point to one record, per collection and level |
| subscription | the scope above, for one uploader's subscription records |
| tag | a copy of any scope above for each tag of the record |
| private | `**` records, for the uploader and for each granted user |

**Sorts** (the order within a scope):

| sort | key part | used for |
|---|---|---|
| `r` | none (the record_id at the end orders it) | default listing, `$uploaded` |
| `i` | index name + typed value | index queries |
| `v` | reversed name + reversed value | ends-with |
| `l` | index name + loose value | loose queries |
| `m` | reversed name + reversed loose value | loose ends-with |
| `k` | index name + Korean initials | initials queries (Hangul values only) |
| `n` | reversed name + reversed initials | initials ends-with |
| `u` | updated time | `$updated` |
| `f` | referenced count | `$referenced_count` |

**Many indexes per record.** `index` is a list `[{name, value}, ...]` with no cap by default; a single
object is also accepted. Each name may appear once per record, so one scan never meets a record twice under
one name. On update, a given `index` list replaces the whole set; `null` removes all of them. Every entry
gets its own sort keys: `i` always, `v` when the value is a string or the name contains `.`, `l` + `m` for
strings, and `k` + `n` for strings with Hangul.

A record writes the **cross product** of its scopes and sorts: the collection and owner scopes always, the
reference scope when it references a record, each of those again once per tag, and in each the `r`, `u`
and `f` keys plus the index keys. Every tag copy carries **every** index key, so a tag combined with any
index condition is still one direct range: nothing is read and thrown away, however rare the tag. The price
is paid on writes ([Scale](#scale)).

Views hold keys only: the record body is read once from `rec`. Changing `data` rewrites only the body. On an
update, the writer computes the old and the new key sets from the stored body and writes only the
difference.

**Private records (`**`) and grants.** A private record is also written under the private scope of its
uploader and of each granted user, with every index sort, so a granted user can run index queries on a
private collection. Granting or revoking writes or deletes that user's keys in the grant call itself, so
access starts or ends the moment the call returns.

**Query planner** (`query.rs`). A request maps to one scope and one sort, then ranges:

| condition | range |
|---|---|
| `=` | `[prefix+v, prefix+v+0xFF)` |
| `>` / `<` | within the type prefix, after / before `v` (strict; lexicographic for strings) |
| `>=` / `<=` on numbers and booleans | within the type prefix, `v` included |
| `>=` on strings (**starts with**) | `[prefix+v, prefix+v+0xFF)` |
| `<=` on strings (**ends with**) | starts with the reversed `v`, on the reversed view |
| `range` | `[v1, v2]` inclusive |
| compound `name.` + value | the children: two merged scans, `name.v\0…` and `name.v.…` |
| any of the above + `match: "loose"` | the same range on `l` (`m` for ends with), or on `k` / `n` for initials |

Other planner rules:
- `$user_id` switches to the owner scope.
- A collection query without `access_level` means "every level I can read": one range per level the
  collection actually uses, up to the caller's level, merged in order.
- Each candidate's metadata is loaded for the final access check (private records, agent key limits), the
  same load that returns it.
- `ascending: false` uses reverse iterators.
- A scan over a name prefix (compound children) can meet one record under two of its names; the planner
  keeps only its first key in scan order, checked from the record body, so the rule also holds across pages.
- Paging: `start_key` is an opaque position; `end_of_list` is true when the range is exhausted. `limit`
  defaults to 50, at most 1000.

**Conditions.** `=`, `>`, `<`, `>=`, `<=` (also accepted as `eq`, `gt`, `lt`, `gte`, `lte`), plus `range`.
On numbers and booleans they are ordinary comparisons. On **strings**, `>=` means **starts with** and `<=`
means **ends with**; `>` and `<` stay strict lexicographic comparisons. The catalog listings use the same
meanings for their name conditions.

---

## Writes

`write.rs`. A new record is checked and built first (`prepare_new`: access, level, collection, source rules,
reference target), then its body, every view key, its catalog level mark, its unique id and its outbox
events go into **one batch** (`put_new`), committed atomically.

**Locks.** Record locks are striped: a name (a record id, a reference target, a unique id) hashes to one of
65,536 mutexes. A write takes the stripes of the names it touches, in sorted order (`lock_many`), so two
writers can never wait for each other. Under those locks it checks what must be exact: that the new record
id is free (drawing a new one if not, up to 8 more times), that the unique id is free, and the reference
rules of the target record (`referencing_limit`, `prevent_multiple_referencing`,
`only_granted_can_reference`, `referencing_index_restrictions`).

**`referenced_count`.** The count in the target record's body is updated in the same batch as the
referencing record, because the limit checks depend on it. Only its `f` sort keys are moved later, by the
background worker, which merges a burst of changes into one rewrite. A record referenced many times a second
therefore costs one body write per reference, not a rewrite of every scope and tag; sorting by
`$referenced_count` can lag the true count for a moment, while the count shown on the record is always exact.

**Grouped writes (`postRecords`).** A RocksDB write has a fixed cost (write queue, log append) whatever its
size. So while a `postRecords` request is the only request writing records, its **plain new records** (no
reference, no unique id, not an update) are saved together, up to 1,000 per write (the `Bulk` writer):
- Each waiting record holds its lock stripe until the group is written. Stripes are taken **without
  waiting**; when one is busy, the waiting records are written and their stripes let go first, and only then
  is the stripe waited for. A request never waits while holding a stripe.
- A count of requests that write records (`Db::writing`, counted in `http::dispatch`) tells whether another
  writer is running. As soon as one is, the waiting records are written, and the request goes on one record
  at a time until no other request has written for 100 ms. With many writers, RocksDB's own group commit
  then merges their single-record writes.
- A record with a reference or a unique id, or an update, is written the usual way, after the records before
  it. Each item still succeeds or fails on its own; the answer lists them in item order.
- A group is atomic: after a crash, all of it was saved or none of it.

---

## Access rules

`access.rs`.

**Levels.** A user has an access level from `00` to `99`, set by the admin when the user is added (default
`00`).

**Reads.** A user can read:
- records at levels up to their own
- their own `**` records
- `**` records they were granted
- a `**` record reached by reference, when they uploaded or were granted the referenced record
- a `**` record they hold a private access key for

**Writes.**
- A user can post at levels up to their own, or at `**`.
- Only the uploader can update a record. The admin can update any record.
- `readonly` records can be changed only by the admin.
- Moving a record into or out of `**` is uploader-only.

**Deletes.** The uploader, the admin, or the uploader of the referenced record when
`can_remove_referencing_records` is set. Readonly records can be deleted only by the admin.

**References.** A record decides who may point to it (`source`): `referencing_limit`,
`prevent_multiple_referencing`, `only_granted_can_reference`, `allow_granted_to_grant_others`,
`can_remove_referencing_records`, and `referencing_index_restrictions` (the referencing record's index with
that name must meet the condition). A user must be able to read a record to reference it.

**Subscription collections** need an active, unblocked subscription to the uploader.

**Private access keys.** `hex(expiry) + hex(HMAC-SHA256(secret, record_id \0 user_id \0 expiry))`, with the
server secret in `meta`. A key expires after `private_key_ttl` (default `1h`). `requestPrivateRecordAccessKey`
gives one to a user who can read the record now, or who can read a record that references it.
`ddbd admin secret rotate` makes a new secret, which cancels every private access key at once; sessions
and agent keys are not affected.

**Grants.** At most 100 users per call. Only the uploader can grant, unless `allow_granted_to_grant_others`
is set.

**Agent keys** narrow these rules, never widen them ([Users and network](#users-and-network)).

---

## Background worker

`worker.rs`. One thread drains the outbox in sequence order, applies each event's effects and deletes the
event, in one atomic batch. A crash can neither lose an event nor apply it twice. It reads up to 4,096
events at a time and merges counter changes per catalog key, so a 10,000-record import produces a few
catalog writes, not 10,000.

| event | effect |
|---|---|
| record added / removed / changed level | collection record counts, sizes and per-level counts; the user's record count |
| tag added / removed | tag record counts |
| index added / removed / changed | index counts and number, boolean and string statistics (averages computed when read) |
| unique id added / removed | the unique-id listing |
| referenced count changed | move the record's `f` sort keys in every scope and tag (merged per record) |
| record removed | when records point to it: delete them if `can_remove_referencing_records` is set, else clear their reference |
| record with `upload_to_feed` | add it to the feed of subscribers who asked for the feed |
| record with `notify_subscribers` | one `ntf` row per subscriber who asked to be notified, pushed to their open `listen` streams |
| subscribe / unsubscribe / block | subscriber and subscribed counters; remove feed rows on unsubscribe |
| user deleted | page through that user's records and delete each one (cascading like any delete), then their subscriptions, subscribers, grants and feed |

Other worker rules:
- **Catalog entries** that reach 0 are removed with their reversed and order keys. The worker is the only
  writer of counters in `cat`, so read-modify-write needs no lock.
- **Ordered listings.** For `getIndexes` with `order.by`, the worker keeps one order key per counter and
  moves it when the value changes.
- **Large cascades** are chunked into several events, so one huge user deletion never blocks the queue.
- **Feed rows** are `subscriber \0 record_id`; level, subscription and access are checked when the feed is
  read, and rows of deleted records are dropped then.
- **Notification expiry.** Once a minute, `ntf` rows older than `notify_keep` (default 1 day) are removed.
- **Status.** The terminal status line shows the outbox depth, so you can see when background work is still
  settling.

---

## Users and network

`users.rs`, `agents.rs`, `server/http.rs`.

**Users.** A user row holds the user id (base62, random), username (unique, the login name), access level,
password hash (Argon2id with its own salt), `enc_salt`, created time, and record, subscriber and subscribed
counts. There is no password reset: the admin can delete the user or change their level.

**Network.** HTTP/1.1 with JSON, on one port, served by `hyper` on `tokio`, with `TCP_NODELAY`. By default
the server speaks **plain HTTP**, for running behind something that terminates HTTPS. `init --tls` makes a
self-signed certificate (`rcgen`) and prints its SHA-256 fingerprint so clients can pin it; a real
certificate can be set with `tls_cert` / `tls_key`. On a TLS port, a plain-HTTP request is refused with
`USE_HTTPS` (browsers get a page pointing to the `https://` address).

**Sessions.** `login {username, password}` returns a token `s_` + 43 random base62 characters. Only its
SHA-256 is kept. Sessions live in memory and are saved in `usr`, so they survive a restart; their last use is
saved at most once an hour. A session ends after `session_idle_hours` (default 24) without use. Every other
call sends `Authorization: Bearer <token>`. Each request's work runs on a blocking thread
(`spawn_blocking`), so RocksDB reads and writes never stall the network threads.

**Agent keys.** A user makes keys for agents and scripts, so the password stays with the person.
- `createAgentKey {name, max_level?, read_only?, private?, collections?, expires?}` returns the key
  (`dk_` + 43 random characters) once; only its SHA-256 is stored. The key is long and random, so the fast
  hash is safe and checking it costs no Argon2 time.
- An agent sends `Authorization: Bearer dk_...`; no login.
- Effective level is the lower of the user's level and `max_level`. `read_only` refuses every write.
  `private: false` (default) hides `**` records and grants. `collections` limits queries, posts and listings.
- An agent key cannot create, list or revoke agent keys. Deleting a user deletes their keys.
- A record written with a key carries `agent: {key_id, name}`.

**Failed-login slowdown.** Failed logins are counted per username and per IP. After `login_free_attempts`
failures (default 5), each attempt must wait a delay that doubles from 1 second up to `login_max_delay`
(default 15 minutes); a success clears the count. During a delay the request is refused with `429` and
`retry_after` **before** any Argon2 work, so a flood of guesses cannot eat the CPU. Password checks run in a
pool the size of the CPU count, so a burst of logins queues instead of slowing queries. Counts live in memory.

---

## Administration

**The admin is the operating system user.** No network user ever has admin powers, whatever their level;
`99` is only the highest access level. Admin commands go through a Unix socket in the data folder
(`admin.sock`, mode `0600`), so only the operating system user who started the server can use it
(`server/admin_sock.rs`). The admin shell, `ddbd admin`, speaks to that socket.

**Web admin** (`server/web_admin.rs`, `server/ui.rs`, `ui/`). A browser console served at `/` on the database
port, for the admin only. It is off until the operating system user sets its password with
`ddbd admin web-password` over the socket; the password can never be set or changed from the network.
- Its calls are `POST /v1/adminInfo`, `/v1/adminLogin`, `/v1/adminLogout` and `/v1/admin {cmd}`; web
  admin sessions live in memory only and end after `admin_session_idle_minutes` (default 60).
- Settings edited there are saved to `dadabada.toml`. Limits, keep times and sign-in settings apply at once;
  port, bind, TLS and storage settings on the next start.
- The page, styles and fonts are compiled into the binary. Its content security policy allows scripts,
  styles, fonts and connections from its own origin only, with one exception: connections to
  `https://cdn.broadwayinc.com`, where releases are published. The Tools screen shows the running version
  and asks that address for the newest one, only when the admin clicks **Check for updates**.

**Restart and stop.** `ddbd stop` and `ddbd restart`, the admin socket and the web admin all stop the
server the same way: new requests are refused, requests in hand finish (up to 30 seconds), the outbox
finishes its current batch, and RocksDB closes. A restart starts the program file found at the same path
again, in the same process, so a new build at that path is the one that starts.

**Debug.** `ddbd debug` shows how values are encoded (`sort`, `num`, `loose`, `reverse`, `key`, `id`), to
check encodings by hand.

---

## Client-side encryption

The server never encrypts anything. `whoami` returns `{user_id, username, access_level, enc_salt, kdf: {alg:
"argon2id", m, t, p}}`. The client derives its key with `argon2id(password, enc_salt)` on its own device and
encrypts data before saving it.
- `enc_salt` is random per user and stored on the server, so the same password gives the same key on any
  device.
- The server never sees the derived key.
- If the password is lost, the key is lost, as is the account.

---

## Wire API

Every call is `POST /v1/<method>` with a JSON body; the one exception is the `listen` stream. Methods
(`http::dispatch`):

| method | purpose |
|---|---|
| `login`, `logout`, `whoami` | session |
| `createAgentKey`, `listAgentKeys`, `revokeAgentKey` | agent keys (password sessions only) |
| `postRecord` | create, or update when `record_id` / `unique_id` is given |
| `postRecords` | many at once; `{list, failed: [{index, code, message}]}` |
| `getRecords` | query |
| `deleteRecords` | by id(s) or by query |
| `getCollections`, `getIndexes`, `getTags`, `getUniqueId` | catalog listings |
| `grantPrivateRecordAccess`, `removePrivateRecordAccess`, `listPrivateRecordAccess`, `requestPrivateRecordAccessKey` | private access |
| `subscribe`, `unsubscribe`, `blockSubscriber`, `unblockSubscriber`, `getSubscriptions`, `getFeed` | subscriptions |
| `GET /v1/listen` | live notifications (Server-Sent Events) |

**Record shape:** `record_id, user_id, agent, collection {name, access_level, subscription}, index [{name,
value}], tags, reference, unique_id, source, readonly, referenced_count, uploaded, updated, ip, data`.
Answers are serialized with sorted keys.

**`listen`.** `GET /v1/listen` with the bearer token (or `?token=`, as browsers' `EventSource` cannot set
headers) opens a Server-Sent Events stream. Each event has the notification's sequence number as `id`. After
a disconnect, the client reconnects with `Last-Event-ID` and first receives every stored notification after
it, then live ones. If that id is older than the keep time, the server sends `event: gap` first. A `: ping`
comment every 30 seconds keeps proxies from closing the connection.

**Validation.**
- Names, tags and string index values: up to 256 characters, no control characters; custom index names
  cannot start with `$`.
- Unknown keys in `config`, `config.collection`, `config.source`, `query` and `query.index` are refused, so a
  typo never silently does nothing.
- `data` is any JSON value, with no size cap.
- Indexes and tags per record are unlimited by default; `max_indexes` / `max_tags` cap them (`INDEX_LIMIT`,
  `TAG_LIMIT`). Lowering a cap does not touch existing records.
- Strings are converted to NFC.

---

## Commands, settings and export

`main.rs`, `config.rs`, `shell/`, `engine/transfer.rs`.

```
ddbd init     [--port 7070] [--bind 0.0.0.0] [--data DIR] [--tls]     make a data folder
ddbd start    [--data DIR] [--notify-keep 1d] [--private-key-ttl 1h]
                  [--max-indexes N] [--max-tags N]                      run the server in this terminal
ddbd stop     [--data DIR]                                          stop the running server cleanly
ddbd restart  [--data DIR] [start options]                          stop, then start in this terminal
ddbd admin    [--data DIR] [command ...]                            admin shell over the local socket
ddbd connect  <address> --user NAME | --key dk_... [command ...]    user shell over the network
ddbd debug    <what> ...                                            show how values are encoded
ddbd version
```

`--data` defaults to `./dadabada-data`, or `DADABADA_DATA`. The data folder is created with mode `0700`.

**Settings** (`dadabada.toml` in the data folder; command-line options override them):

| setting | default |
|---|---|
| `port`, `bind` | `7070`, `0.0.0.0` |
| `tls`, `tls_cert`, `tls_key` | off |
| `session_idle_hours` | 24 |
| `admin_session_idle_minutes` | 60 |
| `notify_keep` | `1d` |
| `private_key_ttl` | `1h` |
| `login_free_attempts`, `login_max_delay` | 5, `15m` |
| `max_indexes`, `max_tags` | 0 (unlimited) |
| `log` | `stdout` |
| `rocksdb.block_cache_mb`, `rocksdb.blob_min_kb` | 256, 4 |

Durations are a number with `ms`, `s`, `m`, `h`, `d` or `w`.

**Export / import** (admin shell and web admin). One JSON object per line, each with a `type`:
- `user`: username, user id, level, password hash, `enc_salt`, created; hashes move, so passwords keep
  working after a move
- `record`: the full record shape with its original record id and user id, so references stay valid
- `grant`, `subscription`, `agent_key` (hash only)

Export reads from a RocksDB snapshot, so it is consistent while the server keeps running; a filtered export
also writes the users who uploaded the exported records. Import sends each record through the normal write
path: it is validated, its view keys are built, and catalog counters update in the background. Sessions and
notifications are not exported. **Backup** is a RocksDB checkpoint: a consistent copy of the database files,
taken while the server runs.

---

## Code layout

```
src/
  main.rs              commands (clap), init / start / stop / restart, the start banner
  config.rs            dadabada.toml, defaults, duration parsing
  error.rs             API errors: status, code, message
  store.rs             RocksDB: column families, Store, Batch, the outbox
  debug.rs             ddbd debug
  codec/
    keys.rs            key parts, typed values, scope and sort prefixes
    ids.rs             record ids, user ids, base62
    body.rs            record body: MessagePack metadata + raw JSON
    text.rs            NFC, names, reversing by character and by segment
    loose.rs           loose form (steps 1-9), Korean initials, rule version
  engine/
    mod.rs             Db: the store, settings, record locks, the count of writing requests
    views.rs           record -> every key that makes it findable
    write.rs           create, update, grouped writes, delete, references, grants
    query.rs           getRecords: planner, ranges, merge across levels, paging
    access.rs          read / write / delete rules, private access keys
    catalog.rs         collections, levels, tags, indexes, unique-id listings
    events.rs          outbox event types
    worker.rs          outbox drain: counters, cascades, feed, notifications, user deletion
    users.rs           users, Argon2id, sessions, failed-login slowdown
    agents.rs          agent keys, effective permissions
    subs.rs            subscriptions, feed, notifications
    transfer.rs        export / import as JSON lines
    json.rs            request parsing helpers, fetch options, record JSON
  server/
    mod.rs             the request log (kept in memory for the web admin) and the clock
    http.rs            the HTTP server, sign-in, /v1/<method> dispatch, the status line
    listen.rs          Server-Sent Events
    tls.rs             certificates
    admin_sock.rs      the admin socket protocol
    web_admin.rs       web admin sign-in, settings
    ui.rs              serves the web admin files with their security headers
  shell/
    admin.rs           admin shell
    client.rs          user shell (ddbd connect)
  ui/                  the web admin: index.html, app.js, app.css, theme.js, bundled JetBrains Mono
tools/
  third_party.py           THIRD-PARTY-NOTICES
  bench/                   benchmarks and behaviour tests
install.sh               the installer served as /get/dadabada/v1
```

Main crates: `rocksdb`, `tokio`, `hyper` / `hyper-util`, `rustls` / `tokio-rustls`, `rcgen`, `argon2`,
`sha2` + `hmac`, `serde` / `serde_json`, `rmp-serde`, `toml`, `clap`, `rustyline`, and for loose match
`unicode-normalization`, `caseless`, `unicode-script` and `unicode-properties`.

---

## Build, release and tests

**Behaviour tests.** `tools/bench/http/write/bulk_tests.py` runs scenarios on a fresh server and prints one
JSON line each, so an older and a newer binary can be compared with `diff`: plain imports, mixed requests
(references, reference limits, unique ids, updates, refused items), edge requests, concurrent importers and
single writers, and a `kill -9` during an import.

**Benchmarks.** `tools/bench/http/` measures reads against PostgreSQL, SQLite and MongoDB, all over the same
HTTP path (`gateway/` puts each behind an HTTP server with DADABADA's API shape), and `tools/bench/http/write/`
measures writes. Their READMEs describe each setup.

---

## Scale

- One node, scaled up. RocksDB handles databases far larger than memory.
- Reads take no locks. Writes lock only the record ids, reference targets and unique ids they touch.
- Write cost per record grows with **scopes × (3 + index keys) × (1 + tags)**: each number or boolean index
  adds 1 key, a compound-name number 2, a string 4 (`i`, `v`, `l`, `m`), and a string with Hangul 2 more
  (`k`, `n`). A record with one tag and one number index writes 16 view keys over its 2 scopes; 3 scopes, 2
  tags and 5 string indexes make 207. Tags and indexes multiply, so `max_indexes` / `max_tags` exist to cap
  them where that matters. Keys hold no values and share prefixes, so in practice each costs a few bytes.
- Every key starts with its scope, which leaves room to split the key space across nodes by scope prefix, or
  to ship the write-ahead log to a read replica. Neither exists yet.

---

## Design choices

- **Writer-decided indexing.** Whoever saves a record names its collection, access level, tags and indexes;
  the database builds every key for them at once. No schema, no index to set up, and every query is a key
  range whose cost follows the page it returns, not the size of the collection.
- **Keys over filters.** Tag copies carry every index key, at the cost of more keys per write, so a query
  never reads records it then throws away.
- **One method, one JSON body.** Every call is `POST /v1/<method>`: queries need a JSON body, query values in
  URLs would leak into logs, and one method with one body maps directly onto an agent's tool.
- **`>=` and `<=` on strings mean starts with and ends with**, rather than separate condition names, so the
  same conditions work on every type; the documentation and error messages spell this out.
- **The operating system user is the root of trust.** The admin socket and the web admin password are both
  set from the server's own account, never from the network.
- **No dictionaries in loose match.** Only Unicode's own rules, so every language gets what can be done
  without guessing, and results never depend on a word list.
