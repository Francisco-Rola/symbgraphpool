# Vegeta S1 native-family expansion plan

This plan is generated from the frozen S1 source conflict corpus, callTracer cache, and historical bytecode cache. It is a manual-review queue, **not** an automatic semantic mapping.

## Current coverage

- Source conflict pairs: **556,297/1,190,783 (46.72%)**
- Storage-access coverage: **34.63%**
- Transactions touching mapped storage: **39.38%**
- Candidate unmapped owners analyzed: **50**
- Effective implementation/code clusters: **47**

## Greedy implementation queue

The gain column is exact unique conflict-pair gain after removing overlap with already mapped families and previously selected candidate clusters.

| Rank | Cluster | Owners | Hints | New pairs | Projected coverage | Representative code |
|---:|---|---:|---|---:|---:|---|
| 1 | `runtime:7aa093a267335e721ee2` | 1 | nft-like | 198,054 | 63.35% | `0x83c3864e1b3da7655cdfbcf586296754522c4102` |
| 2 | `runtime:8124cb21a37a8a0592a6` | 1 | fungible-token-like | 75,163 | 69.66% | `0xaf5191b0de278c7286d6c7cc6ab6bb8a73ba2cd6` |
| 3 | `runtime:3b1faaf3ea56fa4301c9` | 1 | nft-like | 52,574 | 74.08% | `0xeae506c1bcd0f77f0802ca630f65bca442ba0bd9` |
| 4 | `runtime:5a9365c99a7bbf9eca7f` | 1 | nft-like | 40,999 | 77.52% | `0x046898045b351b57fb83421869c6e5d8e4bcc089` |
| 5 | `runtime:09fef2514446249689fb` | 2 | nft-like | 30,787 | 80.10% | `0xba3e7dce870b30d5c62deb99951ea235457bdd9e` |
| 6 | `runtime:805ce8e887953d4a8d77` | 3 | nft-like | 29,651 | 82.59% | `0x516677d72c721f992e99033896b237b6123c16ca` |
| 7 | `runtime:da07c2b18e04c7e0af18` | 1 | nft-like | 27,883 | 84.94% | `0x0e6d176b5c50e2600da92c8ea7f4eed178e9bd07` |
| 8 | `runtime:3d5115b00da3b222d8a0` | 1 | nft-like | 25,355 | 87.07% | `0x5eb5babcefea846b220c82f222f00df95934f5f0` |
| 9 | `runtime:4f2263b0e79376591e9b` | 1 | manual | 24,607 | 89.13% | `0xe3d28e90f110db9ccc056240aab5330a609b7c2e` |
| 10 | `runtime:d34227f056876fec931c` | 1 | nft-like | 17,995 | 90.64% | `0x75b4fdcf7946d99d6fb1fa8c979da004b5bfbd33` |
| 11 | `runtime:8c5b67644c9c21873f8d` | 1 | manual | 17,621 | 92.12% | `0xf8d1c60111acea42eda055bc7e7c08ad487004a1` |
| 12 | `runtime:73be3b16d30bb9183567` | 1 | nft-like | 15,519 | 93.43% | `0x2b2fe81487cfdccd2e2cf32819125d9c8bddde01` |
| 13 | `runtime:d3643cd0430d4f171cc3` | 1 | manual | 10,488 | 94.31% | `0x50f210587307f0d0f5a963b77f6d25885567e336` |
| 14 | `runtime:446115c048b415c02a32` | 1 | nft-like | 8,008 | 94.98% | `0x05b1aec1267bb01b5d12c50ab926489805bbcc9c` |
| 15 | `runtime:82ec049c44789cb0b0dc` | 1 | manual | 4,288 | 95.34% | `0xaadc1fe68b8cacfb12a58fe99b24311c32d15e67` |

Candidate-cluster ceiling: **97.11%**. Target: **95.00%**.

## Cluster dossiers

### runtime:7aa093a267335e721ee2a150f933f139f2d3feb9c7758f06070aff00aeca29aa

- Suggested review archetype: **cw721-family-review**
- Interface hints: nft-like
- Exact unique conflict pairs in candidate owners: **198,071**
- Owner pair attributions (can double count): 198,071
- Storage accesses: 113,519
- Representative effective code address: `0x83c3864e1b3da7655cdfbcf586296754522c4102` (delegate-target)
- Runtime code family: `7aa093a267335e721ee2a150f933f139f2d3feb9c7758f06070aff00aeca29aa`
- Owners: `0xa6cd272874ee7c872eb66801eff62784c0b13285`
- Delegate targets: `0x83c3864e1b3da7655cdfbcf586296754522c4102` (11377)
- Top selectors: `0xb03bc27c` (9137), `0xa22cb465` (5878), `0x23b872dd` (4578), `0x42842e0e` (2610), `0x6352211e` (340), `0xb88d4fde` (184), `0x5ee54e23` (8), `0xce3cd997` (8)

### runtime:8124cb21a37a8a0592a60a549f6e579455a9327b813f8076c9369ecbcccce60f

- Suggested review archetype: **cw20-family-review**
- Interface hints: fungible-token-like
- Exact unique conflict pairs in candidate owners: **75,177**
- Owner pair attributions (can double count): 75,177
- Storage accesses: 4,459
- Representative effective code address: `0xaf5191b0de278c7286d6c7cc6ab6bb8a73ba2cd6` (direct-owner)
- Runtime code family: `8124cb21a37a8a0592a60a549f6e579455a9327b813f8076c9369ecbcccce60f`
- Owners: `0xaf5191b0de278c7286d6c7cc6ab6bb8a73ba2cd6`
- Delegate targets: none
- Top selectors: `0xa9059cbb` (928), `0x70a08231` (314), `0x23b872dd` (183), `0x095ea7b3` (101), `0xdd62ed3e` (16), `0x2e15238c` (11), `0x001d3567` (2)

### runtime:3b1faaf3ea56fa4301c960696bd0d0044af6e4a8e0d620f90262fb71e3f5ed0e

- Suggested review archetype: **cw721-family-review**
- Interface hints: nft-like
- Exact unique conflict pairs in candidate owners: **52,583**
- Owner pair attributions (can double count): 52,583
- Storage accesses: 17,673
- Representative effective code address: `0xeae506c1bcd0f77f0802ca630f65bca442ba0bd9` (direct-owner)
- Runtime code family: `3b1faaf3ea56fa4301c960696bd0d0044af6e4a8e0d620f90262fb71e3f5ed0e`
- Owners: `0xeae506c1bcd0f77f0802ca630f65bca442ba0bd9`
- Delegate targets: none
- Top selectors: `0x6ecd2306` (1083), `0xa22cb465` (658), `0x23b872dd` (620), `0x42842e0e` (171), `0xb88d4fde` (50), `0x6352211e` (40), `0x2f6f98e1` (1), `0x37a66d85` (1)

### runtime:5a9365c99a7bbf9eca7fab9fd74f81e11ffdd2d6919b9aa0c37088bb2b76cb31

- Suggested review archetype: **cw721-family-review**
- Interface hints: nft-like
- Exact unique conflict pairs in candidate owners: **41,026**
- Owner pair attributions (can double count): 41,026
- Storage accesses: 34,288
- Representative effective code address: `0x046898045b351b57fb83421869c6e5d8e4bcc089` (delegate-target)
- Runtime code family: `5a9365c99a7bbf9eca7fab9fd74f81e11ffdd2d6919b9aa0c37088bb2b76cb31`
- Owners: `0xbd18e233e12f2a066f5b5a351285ab5a39b1f2ac`
- Delegate targets: `0x046898045b351b57fb83421869c6e5d8e4bcc089` (4958)
- Top selectors: `0x23b872dd` (2662), `0xa22cb465` (2592), `0x42842e0e` (1640), `0xc96602d9` (1500), `0x29a0eee8` (1128), `0x6352211e` (322), `0xb88d4fde` (50), `0x01ffc9a7` (14)

### runtime:09fef2514446249689fb54873f60d0c2582599db1992e207391345313d70e316

- Suggested review archetype: **cw721-family-review**
- Interface hints: nft-like
- Exact unique conflict pairs in candidate owners: **30,804**
- Owner pair attributions (can double count): 30,804
- Storage accesses: 23,043
- Representative effective code address: `0xba3e7dce870b30d5c62deb99951ea235457bdd9e` (delegate-target)
- Runtime code family: `09fef2514446249689fb54873f60d0c2582599db1992e207391345313d70e316`
- Owners: `0xf66ef61f504a6d326d7bf1771f4b613af57c7126`, `0x925fe29ff5db1614e1344c803543ccbf60fd1641`
- Delegate targets: `0xba3e7dce870b30d5c62deb99951ea235457bdd9e` (3013)
- Top selectors: `0xa0712d68` (2240), `0x23b872dd` (1986), `0xa22cb465` (1046), `0x42842e0e` (466), `0x6352211e` (264), `0xb88d4fde` (8), `0xcc47a40b` (8), `0x3ccfd60b` (4)

### runtime:805ce8e887953d4a8d77dd00c4f04520c222819d5e226dc49141119b87cf0ad3

- Suggested review archetype: **cw721-family-review**
- Interface hints: nft-like
- Exact unique conflict pairs in candidate owners: **29,651**
- Owner pair attributions (can double count): 29,651
- Storage accesses: 14,279
- Representative effective code address: `0x516677d72c721f992e99033896b237b6123c16ca` (delegate-target)
- Runtime code family: `805ce8e887953d4a8d77dd00c4f04520c222819d5e226dc49141119b87cf0ad3`
- Owners: `0x02eb6029adcb09bdc0bde2e61614be415689eabd`, `0x7974e0b19d8ee4daf3fdfecb2420507c198d3dbe`, `0x798116c6858dc4be729820d36554c4c427629744`
- Delegate targets: `0x516677d72c721f992e99033896b237b6123c16ca` (1472)
- Top selectors: `0x0d1d7ae5` (1294), `0xdb980f4f` (894), `0xa22cb465` (386), `0x23b872dd` (136), `0x9ff70755` (134), `0xba09f3d7` (60), `0x199bdb17` (10), `0x528307c8` (8)

### runtime:da07c2b18e04c7e0af18249dc6b14d504b7201089d68efa1885782c810a25333

- Suggested review archetype: **cw721-family-review**
- Interface hints: nft-like
- Exact unique conflict pairs in candidate owners: **27,883**
- Owner pair attributions (can double count): 27,883
- Storage accesses: 9,337
- Representative effective code address: `0x0e6d176b5c50e2600da92c8ea7f4eed178e9bd07` (direct-owner)
- Runtime code family: `da07c2b18e04c7e0af18249dc6b14d504b7201089d68efa1885782c810a25333`
- Owners: `0x0e6d176b5c50e2600da92c8ea7f4eed178e9bd07`
- Delegate targets: none
- Top selectors: `0xa0712d68` (958), `0x23b872dd` (173), `0xa22cb465` (148), `0x42842e0e` (47), `0x3ccfd60b` (1), `0x4891ad88` (1), `0x6352211e` (1), `0x660e35fa` (1)

### runtime:3d5115b00da3b222d8a0cb921049254c86fbe3d16d0dff7f13262ca23e4c7014

- Suggested review archetype: **cw721-family-review**
- Interface hints: nft-like
- Exact unique conflict pairs in candidate owners: **25,356**
- Owner pair attributions (can double count): 25,356
- Storage accesses: 61,181
- Representative effective code address: `0x5eb5babcefea846b220c82f222f00df95934f5f0` (delegate-target)
- Runtime code family: `3d5115b00da3b222d8a0cb921049254c86fbe3d16d0dff7f13262ca23e4c7014`
- Owners: `0x1bbec3ef715cce96b715bc0aa8fef8989f7ad3b2`
- Delegate targets: `0x5eb5babcefea846b220c82f222f00df95934f5f0` (5451)
- Top selectors: `0xefef39a1` (10200), `0xa22cb465` (330), `0x23b872dd` (300), `0x42842e0e` (66), `0xb88d4fde` (4), `0x6352211e` (2)

### runtime:4f2263b0e79376591e9b941152480fb308cfb1b85e8ab4140413e9e99beae6c5

- Suggested review archetype: **manual-review**
- Interface hints: none / manual review
- Exact unique conflict pairs in candidate owners: **24,607**
- Owner pair attributions (can double count): 24,607
- Storage accesses: 10,320
- Representative effective code address: `0xe3d28e90f110db9ccc056240aab5330a609b7c2e` (direct-owner)
- Runtime code family: `4f2263b0e79376591e9b941152480fb308cfb1b85e8ab4140413e9e99beae6c5`
- Owners: `0xe3d28e90f110db9ccc056240aab5330a609b7c2e`
- Delegate targets: none
- Top selectors: `0xa0712d68` (927), `0xa22cb465` (16), `0x6115b360` (1), `0x6f8b44b0` (1), `0xcf18663f` (1)

### runtime:d34227f056876fec931cd609a6ad84696d6afc5cf215618b55ee038d3efb6e63

- Suggested review archetype: **cw721-family-review**
- Interface hints: nft-like
- Exact unique conflict pairs in candidate owners: **17,995**
- Owner pair attributions (can double count): 17,995
- Storage accesses: 4,636
- Representative effective code address: `0x75b4fdcf7946d99d6fb1fa8c979da004b5bfbd33` (direct-owner)
- Runtime code family: `d34227f056876fec931cd609a6ad84696d6afc5cf215618b55ee038d3efb6e63`
- Owners: `0x75b4fdcf7946d99d6fb1fa8c979da004b5bfbd33`
- Delegate targets: none
- Top selectors: `0x26092b83` (418), `0x23b872dd` (177), `0xa22cb465` (103), `0xb88d4fde` (7), `0x42842e0e` (2), `0x18160ddd` (1)

### runtime:8c5b67644c9c21873f8d2e7d3720eff65022f537e16be07806068539a7e3996a

- Suggested review archetype: **manual-review**
- Interface hints: none / manual review
- Exact unique conflict pairs in candidate owners: **17,621**
- Owner pair attributions (can double count): 17,621
- Storage accesses: 11,923
- Representative effective code address: `0xf8d1c60111acea42eda055bc7e7c08ad487004a1` (delegate-target)
- Runtime code family: `8c5b67644c9c21873f8d2e7d3720eff65022f537e16be07806068539a7e3996a`
- Owners: `0x1b1d2dccc2d3f25d7791e9dc4751856ec5eeafaa`
- Delegate targets: `0xf8d1c60111acea42eda055bc7e7c08ad487004a1` (957)
- Top selectors: `0x2955a21d` (1858), `0xa22cb465` (52), `0xc204642c` (2), `0x60806040` (1), `0x6e49aa0a` (1)

### runtime:73be3b16d30bb9183567ac31a32d0e15ef4a2a1a59dbc46229222df52f1b4d31

- Suggested review archetype: **cw721-family-review**
- Interface hints: nft-like
- Exact unique conflict pairs in candidate owners: **15,520**
- Owner pair attributions (can double count): 15,520
- Storage accesses: 11,846
- Representative effective code address: `0x2b2fe81487cfdccd2e2cf32819125d9c8bddde01` (direct-owner)
- Runtime code family: `73be3b16d30bb9183567ac31a32d0e15ef4a2a1a59dbc46229222df52f1b4d31`
- Owners: `0x2b2fe81487cfdccd2e2cf32819125d9c8bddde01`
- Delegate targets: none
- Top selectors: `0xa0712d68` (536), `0x23b872dd` (465), `0xa22cb465` (334), `0x42842e0e` (77), `0xb88d4fde` (5), `0x01ffc9a7` (3), `0x6352211e` (3), `0xc9bd5305` (2)

### runtime:d3643cd0430d4f171cc3ef19b1c3cf4f610ae98a7861cf2e13facc4ec0621361

- Suggested review archetype: **manual-review**
- Interface hints: none / manual review
- Exact unique conflict pairs in candidate owners: **10,488**
- Owner pair attributions (can double count): 10,488
- Storage accesses: 4,496
- Representative effective code address: `0x50f210587307f0d0f5a963b77f6d25885567e336` (direct-owner)
- Runtime code family: `d3643cd0430d4f171cc3ef19b1c3cf4f610ae98a7861cf2e13facc4ec0621361`
- Owners: `0x50f210587307f0d0f5a963b77f6d25885567e336`
- Delegate targets: none
- Top selectors: `0xa0712d68` (421), `0xa22cb465` (33), `0x23b872dd` (10), `0x01ffc9a7` (3), `0x60806040` (1), `0x7ba5e621` (1), `0x8da5cb5b` (1)

### runtime:446115c048b415c02a32d2704da87bb6a1fd531baf853817cff0da1eee79bc2d

- Suggested review archetype: **cw721-family-review**
- Interface hints: nft-like
- Exact unique conflict pairs in candidate owners: **8,008**
- Owner pair attributions (can double count): 8,008
- Storage accesses: 9,521
- Representative effective code address: `0x05b1aec1267bb01b5d12c50ab926489805bbcc9c` (direct-owner)
- Runtime code family: `446115c048b415c02a32d2704da87bb6a1fd531baf853817cff0da1eee79bc2d`
- Owners: `0x05b1aec1267bb01b5d12c50ab926489805bbcc9c`
- Delegate targets: none
- Top selectors: `0x64869dad` (977), `0x840e15d4` (977), `0xa22cb465` (172), `0x23b872dd` (136), `0xb88d4fde` (5), `0x42842e0e` (4), `0xb81095a0` (2)

### runtime:82ec049c44789cb0b0dc0155f3794f3ac1c9756846e1748b9ef1dfe88ed7b7f3

- Suggested review archetype: **manual-review**
- Interface hints: none / manual review
- Exact unique conflict pairs in candidate owners: **4,288**
- Owner pair attributions (can double count): 4,288
- Storage accesses: 3,153
- Representative effective code address: `0xaadc1fe68b8cacfb12a58fe99b24311c32d15e67` (direct-owner)
- Runtime code family: `82ec049c44789cb0b0dc0155f3794f3ac1c9756846e1748b9ef1dfe88ed7b7f3`
- Owners: `0xaadc1fe68b8cacfb12a58fe99b24311c32d15e67`
- Delegate targets: none
- Top selectors: `0x9e852f75` (224), `0xa22cb465` (11), `0x23b872dd` (10), `0x2db11544` (8), `0x34c73884` (2), `0x60806040` (1), `0xa0bcfc7f` (1)

### runtime:6f90c20369339bb1e816d256eaa75fb6c76ee90539fd19af48dfb0b770cf58ef

- Suggested review archetype: **cw20-family-review**
- Interface hints: fungible-token-like
- Exact unique conflict pairs in candidate owners: **9,542**
- Owner pair attributions (can double count): 9,542
- Storage accesses: 20,657
- Representative effective code address: `0x8598e892be45770c5dba939259eb92e2c33484bb` (direct-owner)
- Runtime code family: `6f90c20369339bb1e816d256eaa75fb6c76ee90539fd19af48dfb0b770cf58ef`
- Owners: `0x8598e892be45770c5dba939259eb92e2c33484bb`
- Delegate targets: none
- Top selectors: `0x70a08231` (1492), `0xa9059cbb` (667), `0x095ea7b3` (324), `0x23b872dd` (278), `0x` (162), `0x8a8c523c` (2), `0x60c06040` (1), `0x751039fc` (1)

### runtime:88ce9667b8159948add417bd4841200ab7a25d51cb9e2a87ba5079b5d2c171c7

- Suggested review archetype: **cw20-family-review**
- Interface hints: fungible-token-like
- Exact unique conflict pairs in candidate owners: **3,802**
- Owner pair attributions (can double count): 3,802
- Storage accesses: 22,304
- Representative effective code address: `0x87ebe228e3e4d4e4d24f144d85f6a12f26d48fc1` (direct-owner)
- Runtime code family: `88ce9667b8159948add417bd4841200ab7a25d51cb9e2a87ba5079b5d2c171c7`
- Owners: `0x87ebe228e3e4d4e4d24f144d85f6a12f26d48fc1`
- Delegate targets: none
- Top selectors: `0x70a08231` (2046), `0xa9059cbb` (609), `0x23b872dd` (466), `0x095ea7b3` (312), `0x` (205), `0x1816467f` (1), `0x60c06040` (1), `0x715018a6` (1)

### runtime:92ba07e5b9a121271e3cb6ff1984114eab00dd64f05bd5701682da1a0f3d6afb

- Suggested review archetype: **manual-review**
- Interface hints: none / manual review
- Exact unique conflict pairs in candidate owners: **3,334**
- Owner pair attributions (can double count): 3,334
- Storage accesses: 35,263
- Representative effective code address: `0x88e6a0c2ddd26feeb64f039a2c41296fcb3f5640` (direct-owner)
- Runtime code family: `92ba07e5b9a121271e3cb6ff1984114eab00dd64f05bd5701682da1a0f3d6afb`
- Owners: `0x88e6a0c2ddd26feeb64f039a2c41296fcb3f5640`
- Delegate targets: none
- Top selectors: `0x128acb08` (4326), `0x3850c7bd` (591), `0xddca3f43` (174), `0x0dfe1681` (165), `0xd21220a7` (149), `0x514ea4bf` (138), `0xa34123a7` (74), `0x4f1eb3d8` (72)

### runtime:5aaa8327c5765ec883224895ca02cade2871e12dad0197bdc791efc91c7ef18d

- Suggested review archetype: **manual-review**
- Interface hints: none / manual review
- Exact unique conflict pairs in candidate owners: **2,998**
- Owner pair attributions (can double count): 2,998
- Storage accesses: 1,098
- Representative effective code address: `0x00000000219ab540356cbb839cbe05303d7705fa` (direct-owner)
- Runtime code family: `5aaa8327c5765ec883224895ca02cade2871e12dad0197bdc791efc91c7ef18d`
- Owners: `0x00000000219ab540356cbb839cbe05303d7705fa`
- Delegate targets: none
- Top selectors: `0x22895118` (263), `0xc5f2892f` (2)

### runtime:260c5d2d47c0cbaafb3152e4e55ec121713d40877a8ce73563d0ef75dfcd57f2

- Suggested review archetype: **cw1155-family-review**
- Interface hints: multi-token-like
- Exact unique conflict pairs in candidate owners: **2,933**
- Owner pair attributions (can double count): 2,933
- Storage accesses: 24,233
- Representative effective code address: `0x9bc90f1ed1a21e8ddca57c0fa0e3cf23f302db6e` (delegate-target)
- Runtime code family: `260c5d2d47c0cbaafb3152e4e55ec121713d40877a8ce73563d0ef75dfcd57f2`
- Owners: `0x760831b9a344bf28a7f0e99b3b5fb660451d6c41`
- Delegate targets: `0x9bc90f1ed1a21e8ddca57c0fa0e3cf23f302db6e` (1082)
- Top selectors: `0x57bc3d78` (2040), `0xa22cb465` (82), `0xf242432a` (40), `0x18160ddd` (2)

### runtime:4b89a8a36ae11767506ee6442c45cdbb0866ab9edf03eff657ff959a40de5689

- Suggested review archetype: **cw20-family-review**
- Interface hints: fungible-token-like
- Exact unique conflict pairs in candidate owners: **2,641**
- Owner pair attributions (can double count): 2,641
- Storage accesses: 4,665
- Representative effective code address: `0x0ebb2257b77aa2ede95c5df3c7c882a1233c3c3b` (direct-owner)
- Runtime code family: `4b89a8a36ae11767506ee6442c45cdbb0866ab9edf03eff657ff959a40de5689`
- Owners: `0x0ebb2257b77aa2ede95c5df3c7c882a1233c3c3b`
- Delegate targets: none
- Top selectors: `0x70a08231` (431), `0xa9059cbb` (182), `0x095ea7b3` (128), `0x23b872dd` (46), `0x` (23), `0xa2a957bb` (2), `0x74010ece` (1), `0x7d1db4a5` (1)

### runtime:3d2e4cf078be66f310f594d1abdc8045ca27c85ada2265bbd197c98225cfa9f6

- Suggested review archetype: **cw721-family-review**
- Interface hints: nft-like
- Exact unique conflict pairs in candidate owners: **2,602**
- Owner pair attributions (can double count): 2,602
- Storage accesses: 4,733
- Representative effective code address: `0x7f44e19520b2d8b6aed17c08e818398ddcdc1696` (direct-owner)
- Runtime code family: `3d2e4cf078be66f310f594d1abdc8045ca27c85ada2265bbd197c98225cfa9f6`
- Owners: `0x7f44e19520b2d8b6aed17c08e818398ddcdc1696`
- Delegate targets: none
- Top selectors: `0xd22b78d6` (222), `0x68700329` (123), `0xa22cb465` (25), `0x01ffc9a7` (3), `0xb0ee9f76` (3), `0x14295774` (1), `0x547520fe` (1), `0x55f804b3` (1)

### runtime:401a99335eea2fccfbb372b730e7535170bc4709079c952d595f66b00334bbaf

- Suggested review archetype: **manual-review**
- Interface hints: none / manual review
- Exact unique conflict pairs in candidate owners: **2,495**
- Owner pair attributions (can double count): 2,495
- Storage accesses: 774
- Representative effective code address: `0xac1e192a284d54f442d97bee44256eef7425d7a1` (direct-owner)
- Runtime code family: `401a99335eea2fccfbb372b730e7535170bc4709079c952d595f66b00334bbaf`
- Owners: `0xac1e192a284d54f442d97bee44256eef7425d7a1`
- Delegate targets: none
- Top selectors: `0xa0712d68` (116), `0xa22cb465` (4), `0x01ffc9a7` (3), `0x375a069a` (3), `0x55f804b3` (1), `0x61012060` (1), `0x8da5cb5b` (1), `0xf4a0a528` (1)

### runtime:f53af772241dba89abf0a5ef917848cfcc539c8e8ea580d3b56af7004d58733a

- Suggested review archetype: **cw20-family-review**
- Interface hints: fungible-token-like
- Exact unique conflict pairs in candidate owners: **2,486**
- Owner pair attributions (can double count): 2,486
- Storage accesses: 6,320
- Representative effective code address: `0xf6c97c553da045100ed27af11589107d90cfd8c4` (direct-owner)
- Runtime code family: `f53af772241dba89abf0a5ef917848cfcc539c8e8ea580d3b56af7004d58733a`
- Owners: `0xf6c97c553da045100ed27af11589107d90cfd8c4`
- Delegate targets: none
- Top selectors: `0x70a08231` (329), `0xa9059cbb` (171), `0x095ea7b3` (93), `0x23b872dd` (86), `0x` (61), `0x60c06040` (1), `0x7571336a` (1), `0x8a8c523c` (1)

### runtime:95eaa8083f1635790c163ca295c07fe2af23830c7e9c8fa7259d2dd314f3beb1

- Suggested review archetype: **cw20-family-review**
- Interface hints: fungible-token-like
- Exact unique conflict pairs in candidate owners: **2,327**
- Owner pair attributions (can double count): 2,327
- Storage accesses: 11,110
- Representative effective code address: `0x602f65bb8b8098ad804e99db6760fd36208cd967` (direct-owner)
- Runtime code family: `95eaa8083f1635790c163ca295c07fe2af23830c7e9c8fa7259d2dd314f3beb1`
- Owners: `0x602f65bb8b8098ad804e99db6760fd36208cd967`
- Delegate targets: none
- Top selectors: `0x70a08231` (5261), `0xa9059cbb` (1978), `0x23b872dd` (617), `0x095ea7b3` (351), `0xdd62ed3e` (34)

### runtime:d3f5835c22b61f8590703911e81184ea13a02c55c5713daa20056fc43d738240

- Suggested review archetype: **cw20-family-review**
- Interface hints: fungible-token-like
- Exact unique conflict pairs in candidate owners: **2,104**
- Owner pair attributions (can double count): 2,104
- Storage accesses: 23,177
- Representative effective code address: `0x5680088d2f60aa50e14cc5bb0146f19e962d7a25` (direct-owner)
- Runtime code family: `d3f5835c22b61f8590703911e81184ea13a02c55c5713daa20056fc43d738240`
- Owners: `0x5680088d2f60aa50e14cc5bb0146f19e962d7a25`
- Delegate targets: none
- Top selectors: `0x70a08231` (2396), `0xa9059cbb` (794), `0x23b872dd` (338), `0x095ea7b3` (314), `0x` (127), `0xdd62ed3e` (3), `0x60c06040` (1), `0x715018a6` (1)

### runtime:7d3cc132c0b2988cd269f32b6fbf67d6b162c35412e61e13b456ade8d6522db9

- Suggested review archetype: **cw20-family-review**
- Interface hints: fungible-token-like
- Exact unique conflict pairs in candidate owners: **2,047**
- Owner pair attributions (can double count): 2,047
- Storage accesses: 13,091
- Representative effective code address: `0x5ad902250f86a1bd7e845e87346ce2e1037a84c5` (direct-owner)
- Runtime code family: `7d3cc132c0b2988cd269f32b6fbf67d6b162c35412e61e13b456ade8d6522db9`
- Owners: `0x5ad902250f86a1bd7e845e87346ce2e1037a84c5`
- Delegate targets: none
- Top selectors: `0x70a08231` (1696), `0xa9059cbb` (456), `0x23b872dd` (294), `0x095ea7b3` (255), `0x` (63), `0x60806040` (1), `0x715018a6` (1), `0x751039fc` (1)

### runtime:9e778d3fc91f2a578f855189038c7043a7eb03ada39146f954946a18093494f8

- Suggested review archetype: **cw20-family-review**
- Interface hints: fungible-token-like
- Exact unique conflict pairs in candidate owners: **1,788**
- Owner pair attributions (can double count): 1,788
- Storage accesses: 25,081
- Representative effective code address: `0x9813037ee2218799597d83d4a5b6f3b6778218d9` (direct-owner)
- Runtime code family: `9e778d3fc91f2a578f855189038c7043a7eb03ada39146f954946a18093494f8`
- Owners: `0x9813037ee2218799597d83d4a5b6f3b6778218d9`
- Delegate targets: none
- Top selectors: `0x70a08231` (4061), `0xa9059cbb` (3499), `0x23b872dd` (1031), `0x095ea7b3` (379), `0xdd62ed3e` (243), `0x40c10f19` (105), `0x` (5)

### runtime:38b5b9103c8b72d725eed67fd62c5c0c337216c3fde4b25346195c756f6a7e24

- Suggested review archetype: **cw20-family-review**
- Interface hints: fungible-token-like
- Exact unique conflict pairs in candidate owners: **1,660**
- Owner pair attributions (can double count): 1,660
- Storage accesses: 2,906
- Representative effective code address: `0x89990ad83447696eef0825be097b5738d021fb9f` (direct-owner)
- Runtime code family: `38b5b9103c8b72d725eed67fd62c5c0c337216c3fde4b25346195c756f6a7e24`
- Owners: `0x89990ad83447696eef0825be097b5738d021fb9f`
- Delegate targets: none
- Top selectors: `0x70a08231` (284), `0xa9059cbb` (91), `0x23b872dd` (79), `0x095ea7b3` (55), `0x` (39), `0x31c2d847` (1), `0x60806040` (1), `0x715018a6` (1)

### runtime:f6bfa85924ba0dd6fa9cc545483d0048ab4a866622417d02ba8480a3761623d2

- Suggested review archetype: **cw20-family-review**
- Interface hints: fungible-token-like
- Exact unique conflict pairs in candidate owners: **1,611**
- Owner pair attributions (can double count): 1,611
- Storage accesses: 7,559
- Representative effective code address: `0x54dc15c741196d268eb17858b63b1277ce439581` (direct-owner)
- Runtime code family: `f6bfa85924ba0dd6fa9cc545483d0048ab4a866622417d02ba8480a3761623d2`
- Owners: `0x54dc15c741196d268eb17858b63b1277ce439581`
- Delegate targets: none
- Top selectors: `0x70a08231` (589), `0x23b872dd` (163), `0xa9059cbb` (147), `0x095ea7b3` (139), `0x` (45), `0xdd62ed3e` (6), `0x293230b8` (1), `0x600c8054` (1)

### runtime:e260b2ce9460814f53958740a4bd121bbfd4e8fde63ae0757a18b6084d3af3db

- Suggested review archetype: **cw20-family-review**
- Interface hints: fungible-token-like
- Exact unique conflict pairs in candidate owners: **1,578**
- Owner pair attributions (can double count): 1,578
- Storage accesses: 3,505
- Representative effective code address: `0x132c47b0f73d06c963080ecdc729960d007dd7d7` (direct-owner)
- Runtime code family: `e260b2ce9460814f53958740a4bd121bbfd4e8fde63ae0757a18b6084d3af3db`
- Owners: `0x132c47b0f73d06c963080ecdc729960d007dd7d7`
- Delegate targets: none
- Top selectors: `0x70a08231` (168), `0xa9059cbb` (75), `0x23b872dd` (57), `0x095ea7b3` (54), `0x` (18), `0x1f53ac02` (1), `0x5d098b38` (1), `0x60806040` (1)

### runtime:39fc205e05345fdf48d4bba5a9b3f60cf352195a5ef7358af0261756438064b8

- Suggested review archetype: **manual-review**
- Interface hints: none / manual review
- Exact unique conflict pairs in candidate owners: **1,565**
- Owner pair attributions (can double count): 1,565
- Storage accesses: 2,696
- Representative effective code address: `0x02e2c2314eee894d6caa1d86c877b117542fbc72` (delegate-target)
- Runtime code family: `39fc205e05345fdf48d4bba5a9b3f60cf352195a5ef7358af0261756438064b8`
- Owners: `0xe8d61f527811d2ace054c9a49616b9110f888785`
- Delegate targets: `0x02e2c2314eee894d6caa1d86c877b117542fbc72` (259)
- Top selectors: `0xa0712d68` (444), `0xa22cb465` (68), `0x3ccfd60b` (2), `0x4c1f646c` (2), `0x627804af` (2), `0x60806040` (1)

### runtime:6f7f6d0bba794af05316b3b5c6cc74790fd90b91e6f0b8d3bd7658942dd409de

- Suggested review archetype: **cw20-family-review**
- Interface hints: fungible-token-like
- Exact unique conflict pairs in candidate owners: **1,561**
- Owner pair attributions (can double count): 1,561
- Storage accesses: 15,226
- Representative effective code address: `0xcb397bf1204d91cc16326fcd52eaf44e7719c965` (direct-owner)
- Runtime code family: `6f7f6d0bba794af05316b3b5c6cc74790fd90b91e6f0b8d3bd7658942dd409de`
- Owners: `0xcb397bf1204d91cc16326fcd52eaf44e7719c965`
- Delegate targets: none
- Top selectors: `0x70a08231` (1783), `0xa9059cbb` (458), `0x23b872dd` (321), `0x095ea7b3` (235), `0x` (129), `0x04401930` (2), `0x293230b8` (1), `0x34c5d2ce` (1)

### runtime:712847808cad552e7d285ccca8656e01a9db893a602cd6691c26e01021f27963

- Suggested review archetype: **cw20-family-review**
- Interface hints: fungible-token-like
- Exact unique conflict pairs in candidate owners: **1,525**
- Owner pair attributions (can double count): 1,525
- Storage accesses: 3,087
- Representative effective code address: `0x474eb08d814c048d92c07dc3d1d382254abff08d` (direct-owner)
- Runtime code family: `712847808cad552e7d285ccca8656e01a9db893a602cd6691c26e01021f27963`
- Owners: `0x474eb08d814c048d92c07dc3d1d382254abff08d`
- Delegate targets: none
- Top selectors: `0x70a08231` (321), `0xa9059cbb` (128), `0x23b872dd` (97), `0x095ea7b3` (87), `0x` (45), `0x293230b8` (1), `0x60806040` (1)

### runtime:d2fb7fc591a328c914beaeecc6659180356f438298d1bdde510212d903dc6169

- Suggested review archetype: **cw20-family-review**
- Interface hints: fungible-token-like
- Exact unique conflict pairs in candidate owners: **1,497**
- Owner pair attributions (can double count): 1,497
- Storage accesses: 6,997
- Representative effective code address: `0x453340929f85c2940029e32202485fce25151d2f` (direct-owner)
- Runtime code family: `d2fb7fc591a328c914beaeecc6659180356f438298d1bdde510212d903dc6169`
- Owners: `0x453340929f85c2940029e32202485fce25151d2f`
- Delegate targets: none
- Top selectors: `0x70a08231` (670), `0xa9059cbb` (184), `0x23b872dd` (160), `0x095ea7b3` (109), `0x` (76), `0x02dbd8f8` (3), `0x66ca9b83` (3), `0xd257b34f` (2)

### runtime:c3cdcfd24a1bbc7f7bc590861b418091bd40e0336481e51493e1ffc0131eaeef

- Suggested review archetype: **cw20-family-review**
- Interface hints: fungible-token-like
- Exact unique conflict pairs in candidate owners: **1,442**
- Owner pair attributions (can double count): 1,442
- Storage accesses: 2,657
- Representative effective code address: `0x365260273b64f21c0568275e54452d494834076c` (direct-owner)
- Runtime code family: `c3cdcfd24a1bbc7f7bc590861b418091bd40e0336481e51493e1ffc0131eaeef`
- Owners: `0x365260273b64f21c0568275e54452d494834076c`
- Delegate targets: none
- Top selectors: `0x70a08231` (196), `0xa9059cbb` (132), `0x095ea7b3` (74), `0x23b872dd` (26), `0x` (13), `0xa2a957bb` (4), `0x8f70ccf7` (2), `0x00b8cf2a` (1)

### runtime:83589060885cd6b139ce4b4ed723653d124a00b50c0fa203dbd5a425cb272bc7

- Suggested review archetype: **cw20-family-review**
- Interface hints: constant-product-amm-pair-like, fungible-token-like
- Exact unique conflict pairs in candidate owners: **1,408**
- Owner pair attributions (can double count): 1,408
- Storage accesses: 9,758
- Representative effective code address: `0xefb47fcfcad4f96c83d4ca676842fb03ef20a477` (direct-owner)
- Runtime code family: `83589060885cd6b139ce4b4ed723653d124a00b50c0fa203dbd5a425cb272bc7`
- Owners: `0xefb47fcfcad4f96c83d4ca676842fb03ef20a477`
- Delegate targets: none
- Top selectors: `0x0902f1ac` (1210), `0x022c0d9f` (1121), `0x0dfe1681` (35), `0x70a08231` (10), `0xd21220a7` (9), `0xa9059cbb` (8), `0x23b872dd` (4), `0x89afcb44` (4)

### runtime:9c8e43b60ccac60a1eba769af4335e42610242562105af248d653be9b176a742

- Suggested review archetype: **cw20-family-review**
- Interface hints: fungible-token-like
- Exact unique conflict pairs in candidate owners: **1,407**
- Owner pair attributions (can double count): 1,407
- Storage accesses: 6,846
- Representative effective code address: `0x1e10a79d7eb35307106feb5f33d3912fadb61889` (direct-owner)
- Runtime code family: `9c8e43b60ccac60a1eba769af4335e42610242562105af248d653be9b176a742`
- Owners: `0x1e10a79d7eb35307106feb5f33d3912fadb61889`
- Delegate targets: none
- Top selectors: `0x70a08231` (910), `0xa9059cbb` (227), `0x23b872dd` (169), `0x095ea7b3` (159), `0x` (50), `0x60806040` (1), `0x715018a6` (1), `0x751039fc` (1)

### runtime:b58d1035880fd46123c43499134ffcf56a3db8427cc283ecfea3180f220687f3

- Suggested review archetype: **cw721-family-review**
- Interface hints: nft-like
- Exact unique conflict pairs in candidate owners: **1,367**
- Owner pair attributions (can double count): 1,367
- Storage accesses: 33,418
- Representative effective code address: `0xe6115ada0452d6c48b292971e656bc07901b53f6` (direct-owner)
- Runtime code family: `b58d1035880fd46123c43499134ffcf56a3db8427cc283ecfea3180f220687f3`
- Owners: `0xe6115ada0452d6c48b292971e656bc07901b53f6`
- Delegate targets: none
- Top selectors: `0xa4d62b0f` (1402), `0xa22cb465` (270), `0x23b872dd` (176), `0xb88d4fde` (28), `0x42842e0e` (11), `0x6352211e` (6), `0x01ffc9a7` (3), `0x60806040` (1)

### runtime:43236e7a971210c2c9d65420e0d5dcbdcfdd1a014ded1de656c0ea7d6a2d6f8a

- Suggested review archetype: **cw20-family-review**
- Interface hints: fungible-token-like
- Exact unique conflict pairs in candidate owners: **1,279**
- Owner pair attributions (can double count): 1,279
- Storage accesses: 18,963
- Representative effective code address: `0x4d224452801aced8b2f0aebe155379bb5d594381` (direct-owner)
- Runtime code family: `43236e7a971210c2c9d65420e0d5dcbdcfdd1a014ded1de656c0ea7d6a2d6f8a`
- Owners: `0x4d224452801aced8b2f0aebe155379bb5d594381`
- Delegate targets: none
- Top selectors: `0xa9059cbb` (3804), `0x23b872dd` (1543), `0x70a08231` (1420), `0x095ea7b3` (361), `0xdd62ed3e` (127), `0x18160ddd` (2), `0x313ce567` (2), `0x95d89b41` (2)

### runtime:51fac6e2fb91caeed5a5585f8d3bb231f8a18548b8bddf7a731cbe4862ff94d1

- Suggested review archetype: **manual-review**
- Interface hints: none / manual review
- Exact unique conflict pairs in candidate owners: **1,268**
- Owner pair attributions (can double count): 1,268
- Storage accesses: 23,037
- Representative effective code address: `0x8430be7b8fd28cc58ea70a25c9c7a624f26f5d09` (delegate-target)
- Runtime code family: `51fac6e2fb91caeed5a5585f8d3bb231f8a18548b8bddf7a731cbe4862ff94d1`
- Owners: `0xff1f2b4adb9df6fc8eafecdcbf96a2b351680455`
- Delegate targets: `0x8430be7b8fd28cc58ea70a25c9c7a624f26f5d09` (2394), `0xa1bba894a6d39d79c0d1ef9c68a2139c84b81487` (49)
- Top selectors: `0x7ff48afb` (4538), `0x20825443` (116), `0xf81cccbe` (116), `0x4bd947a8` (49), `0x12a53623` (18)

### runtime:2167986b94f9167c446f90bcbb4efd8b5f1707750c87f565a0c104f0016036ae

- Suggested review archetype: **cw20-family-review**
- Interface hints: fungible-token-like
- Exact unique conflict pairs in candidate owners: **1,263**
- Owner pair attributions (can double count): 1,263
- Storage accesses: 3,215
- Representative effective code address: `0x0be1e49e1ebc7003d636083ca2392bfb9f416611` (direct-owner)
- Runtime code family: `2167986b94f9167c446f90bcbb4efd8b5f1707750c87f565a0c104f0016036ae`
- Owners: `0x0be1e49e1ebc7003d636083ca2392bfb9f416611`
- Delegate targets: none
- Top selectors: `0x70a08231` (191), `0xa9059cbb` (66), `0x23b872dd` (63), `0x095ea7b3` (56), `0x` (17), `0xc0246668` (2), `0x2e6ed7ef` (1), `0x499b8394` (1)

### runtime:263bfc9aa5df9c8dc745ebcdd742b525c6e853b531f06d0b8e3e7c858c6a629e

- Suggested review archetype: **manual-review**
- Interface hints: none / manual review
- Exact unique conflict pairs in candidate owners: **1,237**
- Owner pair attributions (can double count): 1,237
- Storage accesses: 22,271
- Representative effective code address: `0x11b815efb8f581194ae79006d24e0d814b7697f6` (direct-owner)
- Runtime code family: `263bfc9aa5df9c8dc745ebcdd742b525c6e853b531f06d0b8e3e7c858c6a629e`
- Owners: `0x11b815efb8f581194ae79006d24e0d814b7697f6`
- Delegate targets: none
- Top selectors: `0x128acb08` (2935), `0xd21220a7` (455), `0xddca3f43` (449), `0x0dfe1681` (446), `0x3850c7bd` (182), `0x5339c296` (12), `0x1a686502` (11), `0xd0c93a7c` (11)

### runtime:4067626ad7bf8e6b7b2d8d222609857e7a963e0f330c0d25bb202f559077bfac

- Suggested review archetype: **cw1155-family-review**
- Interface hints: multi-token-like
- Exact unique conflict pairs in candidate owners: **1,177**
- Owner pair attributions (can double count): 1,177
- Storage accesses: 36,384
- Representative effective code address: `0xa801896242e6f7ccbe3736b0cea8dd7a3a62f549` (delegate-target)
- Runtime code family: `4067626ad7bf8e6b7b2d8d222609857e7a963e0f330c0d25bb202f559077bfac`
- Owners: `0x7b1a1bd0dfaea532b90e3906d0bd930996c8b209`
- Delegate targets: `0xa801896242e6f7ccbe3736b0cea8dd7a3a62f549` (2586)
- Top selectors: `0x856e92af` (3502), `0xc30d72d5` (852), `0x98ae99a8` (360), `0xf242432a` (212), `0xa22cb465` (146), `0x2eb2c2d6` (102)

### runtime:59a77a3540d67e9b3856ac94c57b254ad89722308036d8b78186f6a1cbf5117d

- Suggested review archetype: **cw20-family-review**
- Interface hints: fungible-token-like
- Exact unique conflict pairs in candidate owners: **1,145**
- Owner pair attributions (can double count): 1,145
- Storage accesses: 12,425
- Representative effective code address: `0x3ffadab7e92e101f9ae6d0ae06db6aef16da1c58` (direct-owner)
- Runtime code family: `59a77a3540d67e9b3856ac94c57b254ad89722308036d8b78186f6a1cbf5117d`
- Owners: `0x3ffadab7e92e101f9ae6d0ae06db6aef16da1c58`
- Delegate targets: none
- Top selectors: `0x70a08231` (1109), `0xa9059cbb` (379), `0x095ea7b3` (210), `0x23b872dd` (167), `0x` (50), `0x60c06040` (1), `0x8095d564` (1), `0xc17b5b8c` (1)

### runtime:fb89a8c94a5b611e3f3a3d6602a7ea263a45d5297be28a21aca504aa6a1a9476

- Suggested review archetype: **cw20-family-review**
- Interface hints: fungible-token-like
- Exact unique conflict pairs in candidate owners: **1,141**
- Owner pair attributions (can double count): 1,141
- Storage accesses: 4,950
- Representative effective code address: `0x84331252f80eeb4bf12d4e9d9644307ad85019e3` (direct-owner)
- Runtime code family: `fb89a8c94a5b611e3f3a3d6602a7ea263a45d5297be28a21aca504aa6a1a9476`
- Owners: `0x84331252f80eeb4bf12d4e9d9644307ad85019e3`
- Delegate targets: none
- Top selectors: `0x70a08231` (281), `0xa9059cbb` (106), `0x23b872dd` (82), `0x095ea7b3` (77), `0x` (50), `0xa2657778` (2), `0xff935af6` (2), `0x60c06040` (1)

### runtime:6686903769da7b932d627de941e4195fd1f1daa3b44fd976c80fe32fe40f41f0

- Suggested review archetype: **manual-review**
- Interface hints: none / manual review
- Exact unique conflict pairs in candidate owners: **1,136**
- Owner pair attributions (can double count): 1,136
- Storage accesses: 3,261
- Representative effective code address: `0xa39937b53cf21ed48d1cfea01405d0a13a689c13` (direct-owner)
- Runtime code family: `6686903769da7b932d627de941e4195fd1f1daa3b44fd976c80fe32fe40f41f0`
- Owners: `0xa39937b53cf21ed48d1cfea01405d0a13a689c13`
- Delegate targets: none
- Top selectors: `0xa0712d68` (196), `0xa22cb465` (168), `0x23b872dd` (73), `0x01ffc9a7` (3), `0x3ccfd60b` (1), `0x60806040` (1), `0x7ba5e621` (1), `0x8da5cb5b` (1)

## Required review before adding a family

1. Resolve verified source or otherwise identify the implementation semantics for the representative effective-code address.
2. Confirm proxy/delegate storage semantics and whether every clustered owner can share one native **code family** while retaining independent native state instances.
3. Implement only the exercised semantic entrypoints/selectors required by the frozen S1 trace; unsupported paths must remain explicit fallbacks, never silent approximations.
4. Produce genuine symbolic-analysis artifacts for the new native code family.
5. Rerun `run-vegeta-s1-native-coverage.sh` and regenerate this plan. Do not run the publication S1 workload until the configured coverage gates pass.
