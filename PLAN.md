# Dadabada: build plan

Dadabada is a native Linux database server written in Rust. It is one binary that you run in a terminal. It listens on one port, and it keeps the skapi record model: collections, indexes (as many per record as the uploader wants), tags, references, unique ids, access levels, private grants and subscriptions. The person who uploads a record decides its indexing, access level and reference rules. No schema exists anywhere.

The spec this plan answers is `dadabada.md` in this folder.

---

## 1. What carries over from skapi, and what changes

| skapi | Dadabada |
|---|---|
| service + owner in every key | gone. One running process is one database. Only the OS user that owns the data folder can run or administer it |
| table | **collection** |
| access_group 0..99, `private`, aliases `public` / `authorized` / `admin` | **access_level** `"00"`..`"99"` and `"**"`. No aliases, no names |
| anonymous users, level 0 open to anyone | gone. Every request needs a user account, including level 00 |
| user signs up | the admin adds users. Password lost = account lost. The admin can only delete the account |
| project owner / master sees everything | the **admin** (the OS user, on the local admin socket) can list every record. Network users can only query |
| records hold files (`bin`), data offload to S3 | gone. A record holds JSON data only |
| private records, client encryption helpers in the backend | gone. The server only hands each user a salt so the client can derive its own encryption key (section 8) |
| tag, reverse, private-anchor rows written later by streams | every query key is written **in the same atomic batch** as the record, so a query never misses a record that was just saved |
| counters (collections, tags, indexes, users, subscribers) and cascades written by stream Lambdas + SQS | written by a **background worker** that drains a durable outbox stored in the same database (section 6) |
| one index per record | **any number of indexes per record**, each name once (section 4) |
| index search is case-sensitive; NFC/NFD text can miss | every string is stored as Unicode NFC, and string indexes can also be searched with **loose match**: case, accents, width, kana type, spaces and punctuation ignored, Korean partial typing and initials (section 3) |
| one login per user, used by every client | users can also make **agent keys**: revocable, capped in level, optionally read-only and limited to collections (section 7) |
| e-mail / push notifications | gone (no mail stack). `get_notified` becomes a live Server-Sent Events (SSE) stream on the same port, resumable after a disconnect (phase 7) |

All other parts of the skapi record feature set stay:
- create, update, bulk post, get and delete
- `unique_id`, `readonly`
- reference rules: `referencing_limit`, `prevent_multiple_referencing`, `can_remove_referencing_records`, `only_granted_can_reference`, `allow_granted_to_grant_others`, `referencing_index_restrictions`
- the built-in `$uploaded`, `$updated`, `$referenced_count` and `$user_id`
- compound index names (`a.b.c`) and their child listing
- the listings `getCollections`, `getIndexes` (with `order.by`), `getTags` and `getUniqueId`
- grants and private access keys
- subscribe, unsubscribe, block, unblock, `getSubscriptions` and `getFeed`
- deleting a user removes that user's records

Added on top: export / import as JSON lines (section 10), expiring private access keys (section 5), and failed-login slowdown (section 7).

---

## 2. Storage engine choice: RocksDB

Use **RocksDB** through the `rocksdb` crate. It is statically linked, so the result is still one binary.

The whole design rests on a single sorted key space. A DynamoDB GSI becomes a set of extra keys in a byte-sorted store. RocksDB is the strongest fit for that:
- **Atomic write batches across column families.** A record and all of its query keys land together or not at all. This replaces the stream fan-out.
- **Forward and reverse iterators with bounded ranges** give ascending/descending order and starts-with for free.
- **Prefix compression inside data blocks.** Our keys share long prefixes (collection, level, index name), and RocksDB stores each key as "shared bytes + suffix". The copied keys behind every query view therefore cost far less disk than their raw length.
- **LZ4 per block, a shared block cache, checkpoints** for online backup, and a WAL for crash safety.
- **BlobDB for large records.** Record data has no size cap, so the `rec` CF turns on RocksDB's integrated blob files: a value at or over `rocksdb.blob_min_kb` (default 4 KB) is written once to a blob file, and only a small pointer goes into the sorted files. Compaction then moves pointers, not megabytes of JSON. Blob garbage collection reclaims space from updated or deleted records. View keys are never large, so only `rec` uses this.

Alternatives considered:
- **redb**, pure Rust B-tree: lighter to build, but it has a single writer, rewrites pages on every commit and has no prefix compression.
- **fjall**, pure Rust LSM: promising, but younger.
- **sled**: unmaintained.

All storage access goes through one small `Store` trait (get, multi_get, batch write, range iterate). The engine could be swapped later without touching the rest.

Column families:

| CF | key | value |
|---|---|---|
| `rec` | record_id | record body (MessagePack meta + raw JSON data bytes) |
| `ix` | query view keys (section 4) | empty |
| `unq` | unique_id | record_id |
| `usr` | `u`+user_id / `n`+username | user row / user_id |
| `key` | `h`+sha256(agent key) / `u`+user_id+`\0`+key_id | agent key row / empty |
| `acc` | `r`+rec+`\0`+user and `u`+user+`\0`+rec | grant row (granted by, can regrant) |
| `sub` | `t`+target+`\0`+subscriber and `s`+subscriber+`\0`+target | options, blocked |
| `feed` | subscriber+`\0`+level+`\0`+record_id | empty |
| `ntf` | subscriber+`\0`+seq (8B big-endian) | notification JSON + created time |
| `cat` | catalog: collections, tags, indexes, unique-id listing, forward + reversed + order keys | counters |
| `evt` | outbox sequence number (8B big-endian) | event |
| `meta` | settings, server secret, format version | |

---

## 3. Lexicographic encoding (the core of Dadabada)

Every query is one range scan over sorted bytes, with no filtering after the fact. To make that work, every value is encoded so that **byte order = the order the user expects**.

**Separator.** Components are joined with `0x00`. Names, tags, collections and string values are refused when they contain control characters (skapi has the same rule), so `0x00` never appears inside a component. No escaping is needed, and a shorter string always sorts before a longer one that starts with it (`"ab"` < `"abc"`).

**Unicode normalization (NFC).** The same visible text can arrive as different bytes: `é` as one code point (NFC) or as `e` + a combining accent (NFD, common from macOS). Dadabada converts every collection name, tag, index name, string index value, unique_id and username to **NFC** when it saves them and again when it reads a query. The bytes then always match and the lexicographic order is stable. Record `data` is never changed.

**Range end.** Valid UTF-8 never contains the byte `0xFF`. The exclusive end of "everything starting with P" is therefore `P` + `0xFF`. This replaces skapi's `\U0010FFFF` sentinel character with a single byte that can never collide with real data.

**Typed values.** One type byte comes first, so all values of one type sit together and "all numbers" or "all strings" is just a type prefix:

| type | byte | encoding | order |
|---|---|---|---|
| boolean | `0x01` | `0x00` false / `0x01` true | false < true |
| number | `0x02` | f64 as 8 bytes big-endian. Positive values have the sign bit flipped; negative values have all bits flipped | true numeric order, including negative decimals |
| string | `0x03` | UTF-8 bytes as given | code point order (UTF-8 byte order equals code point order) |

Negative decimals sort correctly by construction (skapi's old `!N%` padding mis-sorted `-1.5`; that has since been fixed there too). It is also 8 fixed bytes instead of 16+ padded digits. Integers stay exact up to ±2^53.

**Reversed keys for ends-with.** For every record whose index value is a string, or whose index name contains `.`, a second key is written into the reversed view:
- the name is reversed **per `.` segment**: `band.house.title` becomes `dnab.esuoh.eltit` (the same rule as skapi's `reverse_compound_idx`)
- a string value is reversed **by Unicode character, not byte**, so multi-byte UTF-8 stays valid and keeps sorting correctly: `report.pdf` becomes `fdp.troper`

"ends with `.pdf`" then becomes "starts with `fdp.`" in the reversed view: one prefix scan.

**Loose match.** Exact match compares bytes. Loose match compares a *loosened* form of the text, so people and agents find a value without typing it exactly. For every string index value, Dadabada writes the loose form as an extra key, plus its reverse for ends-with. A query with `"match": "loose"` loosens its own value the same way and scans those keys. Only built-in Unicode rules are used, with no dictionaries, so every language gets what can be done without guessing.

The loose form is made in this order, for stored values and query values alike:

1. **Compatibility forms (NFKC).** Full-width `ＡＢＣ１２` becomes `ABC12`; half-width `ｶﾀｶﾅ` becomes `カタカナ`; ligatures (`ﬁ` → `fi`), circled and superscript digits (`①` `²` → `1` `2`), Roman numeral characters (`Ⅳ` → `IV`) and Arabic presentation forms become their plain letters.
2. **Case folding (Unicode full folding).** `Desk LAMP` → `desk lamp`, `STRASSE` and `straße` → `strasse`, Greek final `ς` → `σ`. This also covers every other cased script: Cyrillic, Armenian, Georgian, Cherokee, Deseret, Adlam and others.
3. **Accents and other marks are removed, only where they are accents.** Combining marks are dropped after letters in Latin, Greek, Cyrillic, Armenian, Hebrew, Arabic and Syriac:
   - `cafe` finds `café`, `Viet` finds `Việt`, `angstrom` finds `Ångström`, `αθηνα` finds `Αθήνα`, `елка` finds `ёлка`
   - Hebrew vowel points (niqqud) and Arabic vowel marks (harakat) are ignored
   - In Indic scripts (Devanagari, Bengali, Tamil and the rest), Thai, Lao, Khmer, Myanmar and Tibetan, marks are vowels that change the word, so they are **kept**. So is the Japanese voicing mark (`が` ≠ `か`).
4. **Letters that have no decomposition are mapped by a small fixed table.** `ø`→`o`, `đ`→`d`, `ł`→`l`, `ħ`→`h`, `ŧ`→`t`, `æ`→`ae`, `œ`→`oe`, `þ`→`th`, `ð`→`d`, Turkish `ı`→`i` (with step 2 and 3, `ISTANBUL` finds `İstanbul`).
5. **Script letter forms.**
   - **Japanese:** katakana → hiragana (`カタカナ` = `かたかな`), small kana → full size (`ゃ` = `や`, `っ` = `つ`).
   - **Arabic, Persian, Urdu:** `أ إ آ ٱ` → `ا`, `ى` and Persian `ی` → `ي`, Persian `ک` → `ك`, `ة` → `ه`, `ؤ` → `و`, `ئ` → `ي`, and the stretching mark `ـ` is removed. Arabic and Persian keyboards then find each other's text.
   - **Hebrew:** final letters become their normal forms (`ך ם ן ף ץ` → `כ מ נ פ צ`).
6. **Digits from every script become ASCII digits.** Examples: Arabic-Indic `٢٠٢٦`, Persian `۲۰۲۶`, Devanagari `२०२६`, Thai `๒๐๒๖` and full-width `２０２６` all become `2026`. Any character Unicode marks as a decimal digit is mapped by its value.
7. **Korean is split into the letters that are typed.** `안녕` → `ㅇㅏㄴㄴㅕㅇ`, and compound vowels and finals split too (`ㅘ` → `ㅗㅏ`, `ㄳ` → `ㄱㅅ`). Double consonants (`ㄲ ㄸ ㅃ ㅆ ㅉ`) stay whole, because they are one key on the keyboard. A word typed halfway is then a plain prefix: `안녀` and `안ㄴ` both find `안녕`.
8. **Spaces, punctuation and invisible characters are removed.**
   - This covers every Unicode space (including the wide CJK space U+3000), tabs and line breaks, every punctuation character (`- _ . , ' " / ( ) ! ? : ;` and the CJK and other-script forms), and invisible characters (zero-width spaces and joiners, variation selectors, soft hyphens).
   - Examples: `newyork` finds `New York`, `email` finds `e-mail`, `usa` finds `U.S.A.`, `oneil` finds `O'Neil`, `20261028` finds `2026-10-28`, `안녕하세요` finds `안녕 하세요`.
   - Symbols and emoji (`★`, `$`, `😀`) are **kept**, so a value made of them still has a loose form.
9. **NFC** at the end, so the bytes are stable.

**Korean initials (초성) key.** For a string value that contains Hangul syllables, one more key holds only each syllable's first consonant, after steps 1-8: `안녕 하세요` → `ㅇㄴㅎㅅㅇ`, and `안녕 world` → `ㅇㄴworld`, plus its reverse. It is detected automatically: when a loose query contains Hangul consonants and no Hangul vowels or whole syllables (`ㅇㄴ`, `ㅇㄴㅎ`, `ㅇㄴworld`), the planner scans the initials key instead of the loose key. Values without Hangul never get this key.

Rules for loose match:
- Every condition works: `=`, `>`, `<`, `>=` (starts with), `<=` (ends with) and `range`. Results come back in loose order.
- If a query is empty after loosening (for example it was only `--`), it is refused with a message saying so; it would otherwise match everything. A stored value that loosens to nothing gets no loose keys.
- Exact match stays the default (`"match": "exact"`), for when `ñ` must not match `n` or `й` must not match `и`.
- Index *names*, collection names and tags are always exact; only string index values have a loose form.
- **Not covered, because it needs dictionaries:** Chinese pinyin (`bj` → 北京), Japanese kanji readings (`とうきょう` → 東京) and simplified ↔ traditional Chinese. With unlimited indexes, an uploader who needs these can store the reading as its own index (`{name: "title_reading", value: "とうきょう"}`). Chinese and Japanese kanji text still gets steps 1, 6, 8 and 9.
- **Not covered, because it needs a different kind of index:** *contains* (finding `녕하` in the middle of `안녕하세요`). Sorted keys match the beginning, and the end through reversed keys.
- **Rule versions.** The loose rules and Unicode tables are versioned (`loose_version` in `meta`). If a Dadabada upgrade changes them, the server says so on `start`, and `dadabada admin rebuild-loose` rewrites the loose and initials keys in the background. Exact keys are never affected.

The catalog uses the same trick. Collection names, tag names, index names and unique ids each get a reversed catalog key, so every listing supports ends-with.

**Time-ordered record ids.** A record_id has 16 characters:
- 7 base62 characters for the millisecond time (fixed width, so string order = time order)
- 5 random characters
- 4 characters hashed from the uploader

This is the skapi layout. It means `$uploaded` needs no key of its own: the record id *is* the upload order. The last 4 characters still give the cheap ownership pre-check.

**Counters inside keys.** `$updated` and `$referenced_count` are 8-byte big-endian u64 values, so numeric order = byte order.

---

## 4. Query views (the replacement for GSIs)

A view key is:

```
<view byte> <scope ...> 0x00 <sort ...> 0x00 <record_id>
```

**Scopes**, which match skapi's partition keys:

| scope | bytes | skapi equivalent |
|---|---|---|
| collection | `C` collection `\0` level | `tbl` |
| owner | `U` user_id `\0` collection `\0` level | `usr_tbl` |
| reference | `R` ref_record_id `\0` collection `\0` level | `ref` |
| subscription collection | the scope above + `\0 S` uploader | `tbl/…/sub/01` |
| tag | any scope above + `\0 T` tag | `!tag` anchors |
| granted private | `P` grantee `\0` collection `\0 **` [+ tag] | `*user*` anchors |

**Sorts:**

| sort | bytes | used for |
|---|---|---|
| `r` | (none, the record_id at the end orders it) | default listing, `$uploaded` |
| `i` | index name `\0` typed value | index queries |
| `v` | reversed name `\0` reversed typed value | ends-with |
| `l` | index name `\0` loose string value | `match: "loose"` queries |
| `m` | reversed name `\0` reversed loose value | loose ends-with |
| `k` | index name `\0` Korean initials | loose initials queries (Hangul values only) |
| `n` | reversed name `\0` reversed initials | loose initials ends-with |
| `u` | updated u64 | `$updated` |
| `f` | referenced count u64 | `$referenced_count` |

**Many indexes per record.** `index` is a list: `[{name, value}, ...]`, with no cap on its length. A single `{name, value}` object is still accepted. Each name may appear only once per record, so one scan can never meet the same record twice under one name. On update, a given `index` list replaces the whole set, and `null` removes all of them.

Every index entry gets its own sort keys: `i` always, `v` when the value is a string or the name contains `.`, `l` + `m` when the value is a string, and `k` + `n` when the string contains Hangul. A query still uses **one** index condition, so it stays one range scan. It can be combined with a tag, a reference and a level, as before. `referencing_index_restrictions` checks the referencing record's index with that name.

A record writes the cross product of its scopes and sorts:
- the collection scope always
- the owner scope always
- the reference scope if the record references another record
- every scope again once per tag
- in each scope, the `r` / `u` / `f` keys once and the index sort keys once per index entry

**Tag copies carry every index key (decided 2026-10-28).** Each tag copy of a scope holds the full set of index keys: exact, reversed, loose, reversed loose and Korean initials. A tag combined with any index condition (`=`, `>`, `<`, `>=` starts with, `<=` ends with, `range`, loose, initials) is therefore one direct range scan, as fast as the same query without a tag, however rare the tag is. Nothing is scanned and thrown away.

The price is writes. A record writes (1 + tags) × (3 + index keys) keys per scope, so tags multiply indexes. Adding a tag writes one more full set of the record's index keys, and changing one string index value rewrites its 4 keys in every tag copy. To stop one record from writing an unreasonable number of keys, the number of indexes and tags per record can each be capped with `max_indexes` and `max_tags` (section 10). Both are unlimited by default.

That is the same set skapi builds with GSIs and anchors. The difference is that here it is one atomic batch, and the keys hold nothing. Every view is keys-only, and the body is read with one `multi_get` from `rec`. This keeps updates cheap: changing `data` rewrites only the body, not the view keys.

On update, the writer computes the old key set and the new key set from the stored body, then deletes only the keys that went away and puts only the new ones.

**Private records (`**`) and grants.** For the uploader and for each granted user, the private record is also written under `P grantee …`. These copies include the index sort, so a granted user can run index queries on a private collection. Skapi's private anchors carry no index, so this is a gap in skapi that Dadabada closes. Granting or revoking access writes or deletes that user's keys in the background (section 6).

**Query planner.** Each request maps to one scope and one sort, then one range:

| condition | range |
|---|---|
| `=` | `[prefix+v, prefix+v+0xFF)` |
| `>` / `<` | bounded scan within the type prefix: values after / before `v` (strict; for strings, lexicographic) |
| `>=` / `<=` on numbers and booleans | bounded scan within the type prefix, `v` included |
| `>=` on strings (**starts with**) | `[prefix+v, prefix+v+0xFF)` |
| `<=` on strings (**ends with**) | starts with the reversed `v`, on the reversed view |
| `range` (between) | `[v1, v2]` inclusive |
| compound `name.` + value | children: two merged scans, `name.v\0…` and `name.v.…` |
| any of the above + `match: "loose"` | the same range on `l` (or `m` for `<=` ends with) with the loose value; on `k` / `n` when the query is Korean initials |

Other planner rules:
- `$user_id` switches to the owner scope.
- A collection query with no `access_level` means "every level I can read". The planner opens one iterator per readable level (at most 101) and merges them with a heap, keeping the requested order. Skapi does not have this merged view.
- `ascending: false` uses reverse iterators.
- A scan over a name prefix (compound children) can meet one record under two of its names, for example `a.b.x` and `a.b.x.z`. The planner keeps only the record's first key in scan order. It checks this from the record body, which lists all of its indexes, so the rule holds across pages too.
- Paging: `start_key` is the last key returned, sent as an opaque base64 string. `end_of_list` is true when the range is exhausted.
- `limit` defaults to 50, with a maximum of 1000 (skapi's values).

**Conditions (the skapi meaning, decided 2026-10-28).** `=`, `>`, `<`, `>=`, `<=`, plus `range`. The names `eq`, `gt`, `lt`, `gte`, `lte` are accepted as the same conditions. On numbers and booleans they are ordinary comparisons. On **strings**, `>=` means **starts with** and `<=` means **ends with**, while `>` and `<` stay strict lexicographic comparisons. There are no separate `starts_with` / `ends_with` conditions. The catalog listings (`getCollections`, `getTags`, `getIndexes`, `getUniqueId`) use the same meanings for their name conditions. The manual and error messages spell this out, so agents are not misled by the symbols.

---

## 5. Access rules

**Levels.** A user has an `access_level` from `00` to `99`, set by the admin when the user is added (default `00`).

**Reads.** A user can read:
- records at levels ≤ their own
- their own `**` records
- `**` records they were granted
- a `**` record reached by reference, when they uploaded or were granted the referenced record (the skapi rule)
- a `**` record they hold a private access key for

**Writes.**
- A user can post at levels ≤ their own, or at `**`.
- Only the uploader can update a record. The admin can update any record.
- `readonly` records can be changed only by the admin.
- Moving a record into or out of `**` is uploader-only.

**Deletes.** The uploader, the admin, or the uploader of the referenced record when `can_remove_referencing_records` is set. Readonly records can be deleted only by the admin.

**Subscription collections.** These need an active, unblocked subscription to the uploader.

**Private access key.** This works as in skapi, with these changes:
- It is `expiry || HMAC(secret, record_id \0 user_id \0 expiry)` with the server secret held in `meta`, instead of a sha256 over the joined string that never expires.
- It expires after the time set at startup with `--private-key-ttl` (default `1h`, also `private_key_ttl` in `dadabada.toml`). An expired key is refused, and the client asks for a new one.
- `dadabada admin secret rotate` makes a new server secret, which cancels every private access key at once. Sessions and agent keys are not affected.

**Agent keys** narrow these rules further, never widen them (section 7).

**Grants.** At most 100 users per call. Only the uploader can grant, unless `allow_granted_to_grant_others` is set.

---

## 6. Background worker (the stream replacement)

Every write batch that changes a record also appends **events** to the `evt` CF, in the same atomic batch. One worker thread drains `evt` in sequence order. It applies each event's effects and deletes the event, again in one atomic batch. A crash can therefore neither lose an event nor apply it twice. There is no SQS, no retry storm and no recursive-loop risk.

| event | worker effect (skapi Lambda it replaces) |
|---|---|
| record added / removed / moved level | collection `number_of_records`, `size`, per-level counts; user record count (StreamRecord `track_table`) |
| tag added / removed | tag `number_of_records` (`track_tag`) |
| index added / removed / changed | index `number_of_records`, `number_count`, `total_number`, `bool_count`, `total_bool`, `string_count`; averages are computed at read time (`track_index`, StreamIndex) |
| catalog entry reaches 0 | delete it and its reversed and order keys (StreamTable / StreamTag / StreamIndex) |
| new catalog name | write the reversed catalog key for ends-with listings (`reverse_proxy.py`) |
| record removed with `referenced_count > 0` | delete referencing records if `can_remove_referencing_records` is set, else clear their reference (`remove_all_referenced`) |
| grant / revoke | write or delete the grantee's `P` keys |
| record with `upload_to_feed` | fan out to `feed` for subscribers with `get_feed` (`handle_subscriptions`) |
| record with `notify_subscribers` | write one `ntf` row per subscriber with `get_notified`, then push it to that subscriber's open SSE streams |
| referenced count changed | rewrite that record's `f` sort keys in every scope and tag. Many changes to one record in a burst are merged into one rewrite with the final count |
| subscribe / unsubscribe / block | subscriber and subscribed counters; remove feed rows on unsubscribe |
| user deleted | page through that user's records (owner scope) and delete each one, which cascades like any other delete. Also remove their subscriptions, subscribers, grants and feed (`remove_user` in admin downstream) |

Other worker rules:
- **Coalescing.** The worker merges counter changes per catalog key across a batch of events (like `coalesce_additive_updates`), so a 10,000-record bulk post produces a few catalog writes, not 10,000.
- **Ordered listings.** For `getIndexes order.by`, the worker keeps one order key per counter: `<counter u64/f64 encoded> \0 collection \0 index`. It moves the order key when a value changes. The worker is the only writer of `cat`, so this read-modify-write needs no lock.
- **Large cascades.** These are chunked into several events, so one huge user deletion never blocks the queue.
- **Notification expiry.** The worker deletes `ntf` rows older than the keep time (`--notify-keep`, default 1 day) with a range delete per subscriber, once a minute.
- **Status.** The terminal status line shows the outbox depth, so you can see when background work is still settling.

**Concurrent writers.** Request handlers take short in-memory locks on the record ids they touch: the record itself, its referenced record, and its unique_id. They then commit one batch. That keeps `referencing_limit`, `prevent_multiple_referencing` and unique_id checks exact. The `referenced_count` **number** in the record body is updated in the same batch as the referencing record, because the limit checks depend on it. Only its `f` **sort keys** are moved by the worker. A popular record referenced many times a second therefore costs one body write per reference, not a rewrite of every scope and tag. The cost is that sorting by `$referenced_count` can lag the true count for a moment; the count shown on each record is always exact.

---

## 7. Users, login, network

**Users.**
- A user row holds: user_id (base62 random), username (unique, login name), access_level, a password hash (Argon2id with its own salt), `enc_salt`, created time, record count, subscriber count and subscribed count.
- No password reset exists. The admin can delete the user or change their level.

**Network.**
- HTTP/1.1 + JSON on one port, using `axum` on `tokio`.
- TLS is on by default. On the first run Dadabada creates a self-signed certificate (`rcgen`) and prints its SHA-256 fingerprint so clients can pin it. A real certificate can be set in the config instead.

**Login.**
- `POST /v1/login {username, password}` returns a random 32-byte session token. Only its hash is stored, and it expires after 24 hours of inactivity.
- Every other call sends `Authorization: Bearer <token>`.

**Agent keys.** A user can create keys for agents and scripts, so the password stays with the person.
- `createAgentKey {name, max_level?, read_only?, private?, collections?, expires?}` returns `{key_id, key}`. The key (`dk_` + 43 random characters) is shown once and only its sha256 is stored. The key is long and random, so the fast hash is safe and checking it costs no Argon2 time.
- An agent sends `Authorization: Bearer dk_...` on every call. No login is needed.
- Effective level = the lower of the user's level and `max_level`.
- `read_only: true` refuses every write: posting, deleting, grants and subscription changes.
- `private: false` (the default) hides the user's `**` records and grants from the agent.
- `collections: [...]` limits queries, posts and catalog listings to those collections.
- `expires` is optional and has no default.
- An agent key cannot create, list or revoke agent keys.
- `listAgentKeys` and `revokeAgentKey {key_id}` are for the user; the admin can list and revoke anyone's. Deleting a user deletes their agent keys.
- Records written through an agent key carry `agent: <key name>` so the owner can see which agent wrote what.

**Failed-login slowdown.** The port faces the internet, and each password check deliberately costs CPU (Argon2id).
- Failed logins are counted per username and per IP. After `login_free_attempts` failures (default 5), each further attempt must wait a delay that doubles from 1 second up to `login_max_delay` (default 15 minutes). A success clears the count.
- During a delay, the request is refused with `429` and `retry_after` **before** any Argon2 work, so a flood of guesses cannot eat the CPU.
- Argon2 runs on a thread pool the size of the CPU count, so a burst of real logins queues instead of slowing queries.
- Counts live in memory and reset when the server restarts.

**Admin access.**
- No network user ever has admin powers, whatever their level. Level `99` is only the highest read/write level.
- The admin is never on the network. Admin commands go through a Unix socket in the data folder, created with mode `0600`, so only the OS user that started Dadabada can use it.
- This is item 1 of the spec: the OS user restriction *is* the admin login.

---

## 8. Client-side encryption support

The server never encrypts anything.

`POST /v1/whoami` returns `{user_id, username, access_level, enc_salt, kdf: {alg: "argon2id", m, t, p}}`. The client derives its key with `argon2id(password, enc_salt)` on its own machine and encrypts data before posting.

Properties:
- `enc_salt` is random per user and stored on the server. The same password gives the same key on any device.
- The server never sees the derived key, because the KDF input pairing is never sent.
- If the password is lost, the key is lost. That matches "password lost = account lost".

---

## 9. Wire API

Every call is `POST /v1/<method>` with a JSON body. Names follow the skapi SDK, with "table" renamed to "collection".

| method | purpose |
|---|---|
| `login`, `logout`, `whoami` | session |
| `createAgentKey`, `listAgentKeys`, `revokeAgentKey` | agent keys (password sessions only) |
| `postRecord` | create, or update when `record_id` / `unique_id` is given |
| `postRecords` | bulk; returns `{list, failed:[{index, code, message}]}` |
| `getRecords` | query |
| `deleteRecords` | by id(s) or by query |
| `getCollections`, `getIndexes`, `getTags`, `getUniqueId` | catalog listings |
| `grantPrivateRecordAccess`, `removePrivateRecordAccess`, `listPrivateRecordAccess`, `requestPrivateRecordAccessKey` | private access |
| `subscribe`, `unsubscribe`, `blockSubscriber`, `unblockSubscriber`, `getSubscriptions`, `getFeed` | subscriptions |
| `listen` (SSE, phase 7) | live notifications for `get_notified` subscribers |

**postRecord example:**

```json
{
  "data": {"title": "Desk lamp", "price": 1200},
  "config": {
    "collection": {"name": "products", "access_level": "05",
                   "subscription": {"is_subscription_record": false, "upload_to_feed": true}},
    "index": [{"name": "shop.price", "value": 1200},
              {"name": "title", "value": "Desk Lamp"},
              {"name": "in_stock", "value": true}],
    "tags": ["sale", "lighting"],
    "unique_id": "sku-1042",
    "reference": "0Q8fX2a9kLmZp3Bc",
    "readonly": false,
    "source": {"referencing_limit": 10, "prevent_multiple_referencing": true,
               "can_remove_referencing_records": false,
               "referencing_index_restrictions": [{"name": "rating", "range": [1, 5]}]}
  }
}
```

**getRecords example:**

```json
{
  "query": {"collection": {"name": "products", "access_level": "05"},
            "index": {"name": "title", "value": ".PDF", "condition": "<=", "match": "loose"},
            "tag": "sale"},
  "fetch": {"limit": 20, "ascending": false, "start_key": null}
}
```

**Response:**

```json
{"list": [ {record} ], "start_key": "base64...", "end_of_list": false}
```

**Live notifications (`listen`).** `GET /v1/listen` with `Authorization: Bearer <token>` opens a Server-Sent Events stream (`Content-Type: text/event-stream`). Each event looks like:

```
id: 000000000000a3f1
event: notify
data: {"from":"4kQ9zP2m","record_id":"0Q8fX2a9kLmZp3Bc","collection":"products","title":"New lamp","body":"..."}
```

- `id` is the notification's sequence number. After a disconnect, the client reconnects with `Last-Event-ID: <id>` and the server first sends every stored notification after that id, then continues live. Browsers' `EventSource` does this on its own; `curl -N` works for scripts and agents.
- Notifications are stored in `ntf` for the keep time (default 1 day, `--notify-keep` on `start`). If the last id the client sends is older than the keep time, the server sends `event: gap` first so the client knows some were dropped.
- The server sends a `: ping` comment every 30 seconds so proxies keep the connection open.
- Only the stream is SSE. Every action still goes through `POST /v1/...`.

**Record shape:** `record_id, user_id, agent, collection {name, access_level, subscription}, index [ {name, value} ], tags, reference, unique_id, source, readonly, referenced_count, uploaded, updated, ip, data`.

Validation follows the skapi SDK:
- names, tags and string index values: up to 256 characters, no control characters
- custom index names cannot start with `$`
- `data` is any JSON value; binary uploads do not exist
- no size cap on `data`
- the number of indexes and tags per record is unlimited by default. `max_indexes` / `max_tags` set caps (section 10). A post or update whose `index` list or `tags` list is longer than the cap is refused with `INDEX_LIMIT` or `TAG_LIMIT` and a message naming the cap. Lowering a cap does not touch existing records; it only applies when a record's indexes or tags are next sent.
- strings are converted to NFC (section 3)

---

## 10. Terminal: commands, arguments, and how it looks

One binary, `dadabada`, with subcommands (`clap`).

```
dadabada init    [--port 7070] [--bind 0.0.0.0] [--data ./dadabada-data] [--no-tls]
dadabada start   [--data DIR] [--notify-keep 1d] [--private-key-ttl 1h]
                 [--max-indexes N] [--max-tags N]
                                              run the server in this terminal
dadabada admin   [--data DIR] [command ...]   admin shell over the local socket
dadabada connect <host:port> --user NAME      user shell over the network
dadabada connect <host:port> --key dk_...     the same shell with an agent key
dadabada version
```

**First run:**

```
$ dadabada init --port 7070
Created data folder /home/ubuntu/dadabada-data (owner: ubuntu, mode 0700)
Saved settings to /home/ubuntu/dadabada-data/dadabada.toml
Made a TLS certificate. Clients can pin this fingerprint:
  SHA256 4F:1A:9C:...:E2
Next: dadabada start --data /home/ubuntu/dadabada-data
```

**Running:**

```
$ dadabada start
Dadabada 0.1.0
  data     /home/ubuntu/dadabada-data
  network  0.0.0.0:7070 (TLS)
  admin    /home/ubuntu/dadabada-data/admin.sock (only OS user ubuntu)
  users 3 · records 12,408 · collections 7
Ready.
14:02:11  alice   postRecord    products        2 ms
14:02:12  bob     getRecords    products  idx   1 ms   20 rows
14:02:15  alice   deleteRecords products        1 ms
[outbox 0 · req/s 4 · conns 2]
```

Ctrl-C stops it cleanly: new requests are refused, the outbox finishes its current batch, and RocksDB closes. A sample systemd unit goes in the manual.

**Admin shell.** This runs as the same OS user, either interactively or as one-shot commands:

```
$ dadabada admin
dadabada admin> user add alice --level 10
Password for alice: correct-horse-battery
Added alice (user id 4kQ9zP2m, level 10).

dadabada admin> user list
USERNAME  USER ID   LEVEL  RECORDS  CREATED
alice     4kQ9zP2m  10     1,204    2026-10-28 14:00
bob       9xLw0aTe  00     88       2026-10-28 14:01

dadabada admin> user level bob 05
dadabada admin> user del alice
This deletes user alice and the 1,204 records alice uploaded.
Type the username to confirm: alice
Deleted alice. Removing records in the background (outbox 1,204).

dadabada admin> records [--collection products] [--user bob] [--level 05] [--limit 50] [--json]
dadabada admin> record get 0Q8fX2a9kLmZp3Bc
dadabada admin> collections [>= text | <= text]      (starts with / ends with)
dadabada admin> status
dadabada admin> backup /mnt/backup/dadabada-2026-10-28
dadabada admin> agents [--user bob]
dadabada admin> agent revoke 7fQ2
dadabada admin> rebuild-loose                (only after an upgrade changes the loose rules)
dadabada admin> secret rotate
This cancels every private access key. Type rotate to confirm: rotate

dadabada admin> export /mnt/backup/all.jsonl [--collection products] [--user bob]
Exported 3 users, 12,408 records, 41 grants, 9 subscriptions, 2 agent keys.
dadabada admin> import /mnt/backup/all.jsonl [--skip-existing | --replace]
Imported 12,408 records (0 skipped). Catalog counters are updating in the background (outbox 12,408).
```

**Export / import format.** One JSON object per line, each with a `type`:
- `user`: username, user_id, level, password hash, enc_salt, created. Hashes move, so passwords keep working after a move.
- `record`: the full record shape, with its original record_id and user_id, so references stay valid.
- `grant`, `subscription`, `agent_key` (hash only).

Export reads from a RocksDB snapshot, so it is consistent while the server keeps running. A filtered export also writes the users who uploaded the exported records. Import sends each record through the normal write path: it is validated, its view keys are built, and catalog counters update in the background. Sessions and notifications are not exported. The format goes in the manual, so a later script can convert skapi records (table to collection, access_group to level).

Passwords are typed in the clear, not hidden.

**User shell.** This is useful for testing and for people. It sends the same JSON calls as any other client.

```
$ dadabada connect db.example.com:7070 --user bob
Certificate fingerprint SHA256 4F:1A:...:E2 - trust it? (yes/no) yes
Password: hunter22
Signed in as bob (level 05).

bob> post products --level 05 --index price=1200 --index title="Desk Lamp" \
       --tags sale,lighting --unique sku-1042 --data '{"title":"Desk lamp"}'
Saved record 0Q8fX2a9kLmZp3Bc

bob> get products --index "price >= 1000" --tag sale --limit 20 --desc
bob> get products --index "title <= .pdf"         (ends with)
bob> get products --index "title >= Desk"         (starts with)
bob> get products --index "title ~<= .PDF"        (loose ends with)
bob> get products --index "title ~>= newyork"     (finds "New York")
bob> get products --index "title ~>= ㅇㄴ"         (finds "안녕하세요")
bob> get products --index "price 100..500"
bob> get products --by '$updated' --desc
bob> get --ref 0Q8fX2a9kLmZp3Bc
bob> get --unique sku-1042
bob> more                                   (next page)
bob> del 0Q8fX2a9kLmZp3Bc
bob> collections | indexes products [--order average_number] | tags products
bob> grant 0Q8fX2a9kLmZp3Bc alice | revoke ... | grants 0Q8fX2a9kLmZp3Bc
bob> subscribe alice --feed | unsubscribe alice | block alice | feed
bob> listen                                 (print notifications as they arrive; Ctrl-C to stop)
bob> agent create shop-bot --level 05 --read-only --collections products --expires 30d
Key (shown once): dk_V3n...q9
bob> agent list | agent revoke 7fQ2
bob> whoami
bob> raw getRecords '{"query":{...}}'       (send any API call as JSON)
```

Shell index syntax: `name OP value` with `OP` in `= > < >= <=` (on strings `>=` is starts with and `<=` is ends with), or `name lo..hi`. Values are numbers if they parse as numbers, `true`/`false` as booleans, otherwise strings. Quoting forces a string. `--index` can be repeated on `post`. A `~` before the operator (`~=`, `~>`, `~<`, `~>=`, `~<=`, `~lo..hi`) makes it a loose match.

**Settings** (`dadabada.toml`): only `port` and `bind` are needed. The rest are optional:
- `tls_cert` / `tls_key`
- `session_idle_hours`
- `notify_keep` (default `1d`; accepts `30m`, `12h`, `7d`; `--notify-keep` on `start` overrides it)
- `private_key_ttl` (default `1h`, same units; `--private-key-ttl` on `start` overrides it)
- `login_free_attempts` (default 5) and `login_max_delay` (default `15m`)
- `rocksdb.blob_min_kb` (default 4)
- `max_indexes` and `max_tags`: caps per record (default `0` = unlimited; `--max-indexes` / `--max-tags` on `start` override them). `status` in the admin shell shows the values in force.
- `rocksdb.block_cache_mb`
- `log` (stdout | file)

---

## 11. Code layout

```
dadabada/
  Cargo.toml
  src/
    main.rs            clap entry, subcommands
    config.rs          dadabada.toml
    store/             Store trait + RocksDB impl, column families
    codec/
      keys.rs          tuple join, typed value encoding, reverse per segment
      ids.rs           record_id, user_id, base62
      body.rs          record body (MessagePack meta + raw JSON)
      text.rs          NFC, reverse by character
      loose.rs         loose form (steps 1-9), Korean initials, rule version
    engine/
      views.rs         record -> full view key set; diff old/new
      write.rs         post / update / bulk / delete, locks, batch + events
      query.rs         planner, range scans, heap merge, paging
      access.rs        read / write / delete rules, private key
      reference.rs     source rules, referenced_count
      catalog.rs       collections, tags, indexes, unique-id listings
      subs.rs          subscriptions, feed
      users.rs         users, Argon2id, sessions, enc_salt
      agents.rs        agent keys, effective permissions
      transfer.rs      export / import JSON lines
      worker.rs        outbox drain, coalescing, cascades
    server/
      http.rs          axum routes /v1/*, TLS
      admin_sock.rs    Unix socket admin protocol
      listen.rs        SSE notifications (phase 7)
      throttle.rs      failed-login slowdown
    shell/
      admin.rs         admin shell (rustyline)
      client.rs        connect shell, argument parser
  MANUAL.md
```

Main crates:
- `rocksdb`, `tokio`, `axum`
- `rustls` / `tokio-rustls`, `rcgen`
- `argon2`, `hmac` + `sha2`
- `serde` / `serde_json`, `rmp-serde`
- `clap`, `rustyline`, `tracing`, `rand`
- `unicode-normalization`, `caseless`, `unicode-script`, `unicode-properties` (general category, for marks, digits, punctuation, spaces)

---

## 12. Build phases

1. **Codec + store.** Key encoding, typed values, NFC, loose form and Korean initials, reversed keys, record ids, the RocksDB CFs with BlobDB on `rec`.
2. **Records.** `postRecord`, `getRecords` and `deleteRecords` for one user: collection / owner scopes, many indexes per record, all sorts, all conditions, exact and loose match, paging, ends-with.
3. **Users + network.** Admin socket, `init` / `start`, user add/list/level/delete, login, failed-login slowdown, TLS, access levels, agent keys.
4. **Background worker.** Outbox, catalog counters, catalog listings with order and ends-with, user-deletion cascade.
5. **References + unique id + source rules.** `referenced_count` (sort keys via the worker), limits, restrictions, cascading deletes, `readonly`, bulk post.
6. **Private access.** `**`, grants, `P` keys, expiring private access keys, `secret rotate`, access by reference.
7. **Subscriptions.** Subscribe/block, subscription collections, feed fan-out, notifications, `listen` SSE stream.
8. **Export / import, shells polish + MANUAL.md** (spec item 11).

**Checking it works.** After each phase, build a release binary and drive it by hand through the admin shell and `dadabada connect`, with a scripted run of the shell commands in section 10. No test suite is added unless you ask for one.

---

## 13. Scale notes

- One node, scaled up. RocksDB handles databases far larger than RAM.
- Reads take no locks. Writes lock only the record ids they touch.
- Write cost per record grows with **scopes × (3 + index keys) × (1 + tags)**, where each number or boolean index adds 1 key, a compound-name number adds 2, a string index adds 4 (`i`, `v`, `l`, `m`), and a string with Hangul adds 2 more (`k`, `n`). Example: 3 scopes, 2 tags, 5 string indexes = 3 × 23 × 3 = 207 keys. Tag copies carry every index key (section 4), so tags and indexes multiply: 4 string indexes and 4 tags is 190 keys over 2 scopes, and 100 of each would be about 81,000 keys in one write. The same growth exists in skapi's GSIs and tag anchors. Indexes and tags are unlimited by default; `max_indexes` / `max_tags` cap them when that matters. Keys are small and prefix-compressed, so in practice this is bytes per key, not a copy of the record.
- Every key starts with its scope. That leaves room for a later phase to split the key space across nodes by scope prefix, or to ship the RocksDB WAL to a read replica. Neither is in v1.

---

## 14. Decisions (2026-10-28)

1. **Record size:** no cap on `data` size. Tags and indexes are unlimited by default, with optional caps (decision 14).
2. **Live notifications:** yes, as Server-Sent Events (`listen`), in v1 (phase 7). Missed notifications are kept 1 day by default, set at startup with `--notify-keep` (or `notify_keep` in `dadabada.toml`).
3. **String conditions:** as in skapi, `>=` on a string means starts with and `<=` means ends with; `>` and `<` are strict lexicographic comparisons. No `starts_with` / `ends_with` condition names (changed 2026-10-28, replacing the earlier explicit names).
4. **Admin:** only the operating system user that owns the data folder, through the local admin socket. No network user has admin powers.
5. **Agent keys:** yes (section 7).
6. **Unicode:** every string is stored and queried as NFC (section 3).
7. **Large records:** BlobDB on `rec`, threshold `rocksdb.blob_min_kb` (section 2).
8. **`$referenced_count` sort keys:** moved by the background worker; the count itself stays exact (section 6).
9. **Private access keys:** expire; `--private-key-ttl` at startup, default `1h`; `secret rotate` cancels all (section 5).
10. **Failed-login slowdown:** yes; `login_free_attempts` 5, `login_max_delay` 15m by default (section 7).
11. **Export / import:** JSON lines through the admin shell (section 10).
12. **Indexes:** any number per record, each name once (section 4).
13. **Loose match:** `"match": "loose"` (shell `~`) on any string index condition. It ignores case, accents, width, kana type, spaces, punctuation and invisible characters, unifies digits and Arabic/Persian/Hebrew letter forms, handles Korean partial typing, and detects Korean initials (`ㅇㄴ`) automatically. Built-in Unicode rules only, no dictionaries (section 3).
14. **Tag copies and caps:** every tag copy carries every index key, so tag + index queries are always one range scan (section 4). Caps per record are configurable: `max_indexes` / `max_tags` in `dadabada.toml` or `--max-indexes` / `--max-tags` on `start`, default unlimited (section 10).

15. **API style (2026-10-29):** keep `POST /v1/<method>` with a JSON body for every call. REST verbs (GET/PATCH/DELETE routes) were considered and not added: queries need a JSON body, URLs leak query values into logs, and one-method-one-body maps directly onto agent tools.

16. **Web admin (2026-10-29):** a browser admin console served at `/` on the database port, admin only. Its password is set, changed or turned off only by the OS user (`dadabada admin web-password`), so the OS user stays the root of trust; web admin is off until then. Settings edited there are saved to `dadabada.toml`; limits, keep times and sign-in settings apply at once, port/bind/TLS/storage on the next start. Brand from `design/`: ink on paper, JetBrains Mono (bundled), three-wave mark. Rebuilt 2026-10-29 on the skapi.com dashboard structure (top bar, side nav, list-page kit, bq-meta cards, bq-adv forms, delete modal) in monotone, with skapi's dark tokens re-tuned to neutral greys and a light/dark toggle.

---

## 15. Build notes (2026-10-29)

Built as planned, in `src/` (Rust, RocksDB 0.25). Where the build differs from the plan above:

- **Grants write their `P` keys immediately**, in the grant call itself, not through the background worker. Grants are capped at 100 users per call, so this is small, and a granted user can query the record the moment the call returns.
- **Feed rows** are `subscriber \0 record_id` (no level in the key). The level, subscription and access are checked when the feed is read, and rows for deleted records are dropped then.
- **`dadabada debug`** was added to check encodings by hand: `debug sort`, `debug num`, `debug loose`, `debug reverse`, `debug key`, `debug id`.
- **Shell extras for scripting:** `connect ... --password P --trust <command>` runs one command and exits; `admin ... <command>` does the same over the socket. Notification text on `post` is `--notification "title|body"` (`--notify` is the subscribe switch).
- **Admin `records --collection`** scans the record store and filters, so it also lists subscription-only records.
- **Sessions** live in memory and are saved in the `usr` column family, so they survive a restart. The idle timer is saved at most once an hour per session.
- **Private access keys** are `hex(expiry) + hex(HMAC-SHA256(secret, record_id \0 user_id \0 expiry))`. `requestPrivateRecordAccessKey` gives a key to a user who can read the record now, or who can read a record (`reference_id`) that references it.

Checked by running the server (TLS and plain HTTP) and driving it with `dadabada connect`, `dadabada admin` and curl: every query condition, loose match in several scripts, tags, compound names, built-in indexes, paging across levels, access levels, agent keys, login slowdown, catalog counters, every referencing rule and both delete cascades, readonly, grants, private access keys (including expiry and `secret rotate`), subscriptions, subscription records, blocking, feed, SSE live and resume with `gap`, user deletion cleanup, export/import, backup, caps, `rebuild-loose`, a 2 MB record, and a clean Ctrl-C stop and restart.
