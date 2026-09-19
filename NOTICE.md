# Licensing and attribution

The software and the generated dictionary data are separate works with separate licenses.

## Creator software

Syng Dictionary Creator is Copyright (C) 2024–2026 Preston Wang-Stosur-Bassett and is licensed under GPL-3.0-only. See `LICENSE`. Its Rust dependencies use GPL-compatible open-source licenses; `deny.toml` is the reviewed allowlist used to detect dependency-license drift.

## Generated dictionary bundle

The generated bundle's CC-licensed adapted material is licensed under [CC BY-SA 4.0](https://creativecommons.org/licenses/by-sa/4.0/). WordNet-derived morphology remains under the separate Princeton WordNet license in `LICENSE-WORDNET.txt`. See `LICENSE-DATA`. CC BY-SA 3.0 permits an adapter to apply a later BY-SA license with the same license elements, allowing Chinese Notes material to be combined with the CC BY-SA 4.0 sources under this adapter's license.

The build materially changes the sources: it parses, normalizes, filters, structurally annotates, deduplicates exact matches, merges data, assigns stable identities, and generates indexes. Wiktionary quotations are deliberately excluded. No upstream project or contributor endorses Syng or the resulting bundle.

Every generated bundle contains its own `NOTICE.md`, `LICENSE-DATA.txt`, `LICENSE-WORDNET.txt`, `manifest.json`, and `wiktionary-attribution.json`. Those files are part of the distribution and must travel with the data. The manifest pins exact revisions and checksums. The attribution index links each lexical identity containing Wiktionary material to the relevant English Wiktionary entry pages and contributor histories.

## Upstream data

- CC-CEDICT is a community-maintained Chinese-English dictionary published by MDBG under [CC BY-SA 4.0](https://creativecommons.org/licenses/by-sa/4.0/). It references CEDICT, Copyright (C) 1997, 1998 Paul Andrew Denisowski. [License evidence and downloads](https://cc-cedict.org/editor/editor.php?handler=Download).
- The Chinese Notes word dictionary is Copyright Fo Guang Shan 佛光山 2013–2025 and is offered under [CC BY-SA 3.0](https://creativecommons.org/licenses/by-sa/3.0/). The project is maintained by Alex Amies and acknowledges upstream contributors and reference works. [License evidence](https://chinesenotes.com/about).
- English Wiktionary entry text is copyright its respective contributors and is used under its [CC BY-SA 4.0](https://creativecommons.org/licenses/by-sa/4.0/) option. The source is structured by Tatu Ylonen's Wiktextract and distributed by Kaikki.org. Wiktionary also offers its material under the GFDL; this bundle does not rely on that option. [Wiktionary copyright terms](https://en.wiktionary.org/wiki/Wiktionary:Copyrights) and [Kaikki license notice](https://kaikki.org/dictionary/).
- Princeton WordNet 3.1 is Copyright 2011 by Princeton University and is used under the [WordNet License](https://wordnet.princeton.edu/license-and-commercial-use). The generated English morphology mappings are adapted from its noun and verb lemmas and exception lists. The complete notice and disclaimer are reproduced in `LICENSE-WORDNET.txt`; WordNet is a registered trademark, and this project does not imply Princeton endorsement. Princeton also requests an appropriate WordNet citation; see [Citing WordNet](https://wordnet.princeton.edu/citing-wordnet).

The authoritative, revision-specific attribution strings, source URLs, license URLs, evidence URLs, copyright notices, and change descriptions are maintained in `sources.lock.json` and copied into each generated manifest and notice.

This is an engineering compliance record, not legal advice. Upstream licensors can only license rights they hold; suspected source-level infringement should be reported to the relevant upstream project and excluded from future pinned builds while investigated.
