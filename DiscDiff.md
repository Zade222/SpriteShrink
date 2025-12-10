It is actually very difficult to mistake a PS1 game for a Sega CD or PC Engine game if you look at the raw bytes, primarily because Sony was the only one of that era to exclusively use the CD-XA (Mode 2) format for the entire disc.
Both the Sega CD and the PC Engine CD adhere to the older Mode 1 standard (the same standard used by PC data discs).
Here is how you can instantly tell them apart in a hex editor.
1. The "Mode Byte" Check (The Fastest Method)
This is the single most reliable technical indicator. As we discussed, look at the 16th byte (Offset 15) of any sector.
| Console | Sector Mode | Byte at Offset 15 |
|---|---|---|
| PlayStation 1 | Mode 2 (XA) | 02 |
| Sega CD | Mode 1 | 01 |
| PC Engine CD | Mode 1 | 01 |
| Sega Saturn | Mode 1 (Mostly) | 01 |
 * If you see 02: It is likely PS1 (or CD-i).
 * If you see 01: It is definitely not a standard PS1 game; it is likely Sega, PC Engine, or a PC Data disc.
2. Identifying Sega CD (The "Security Code")
Sega CD discs are standard ISO 9660 Mode 1 discs, but they have a massive, proprietary header in the first 16 sectors (the "System Area") that acts as a region lock and boot identifier.
Where to look: The very first few bytes of the file (Sector 0).
What to look for:
 * Offset 0: SEGADISCSYSTEM (ASCII)
 * Offset 0x100: SEGA GENESIS or SEGA MEGA DRIVE
The ISO Identifier Offset:
Because Sega CD uses Mode 1 (16-byte headers), the "CD001" string will be at offset 0x9311 (37,649), not 0x9319 like the PS1.
3. Identifying PC Engine / TurboGrafx-CD
These are the "wild west" of CD formats. They generally do not use the ISO 9660 file system. They treat the CD as a raw stream of data sectors and the game code loads by asking for "Sector 100," not "File.exe."
Where to look: Sector 1 (Offset 0x800 or 0x930 depending on raw/cooked dump).
What to look for:
 * You will usually find the string PC Engine or PC-Engine very early in the boot sector code.
 * Lack of CD001: If you search for CD001 and find nothing, but the disc has data, it is a strong candidate for PC Engine (or very early Japanese computer games).
4. The "Sega Saturn" Trap
The Sega Saturn is the console most easily confused with PS1 because they are from the same generation, but even the Saturn stuck to Mode 1 for its data tracks.
Where to look: Sector 0 (The "IP.BIN").
What to look for:
 * Top of the file: SEGA SEGASATURN
Summary Table for Hex Identification
| Feature | PlayStation 1 | Sega CD | PC Engine CD |
|---|---|---|---|
| Sector Header | Mode 2 (02 at byte 15) | Mode 1 (01 at byte 15) | Mode 1 (01 at byte 15) |
| Subheaders? | Yes (8 bytes) | No | No |
| ISO 9660? | Yes (CD001 at 0x9319) | Yes (CD001 at 0x9311) | No (Usually) |
| System String | Licensed by Sony... | SEGADISCSYSTEM | PC Engine |
| Region | Checked in Exe / Sector 4 | Checked in Sector 0 Header | Mostly Region Free (System Card checks) |
Conclusion:
You cannot mistake them. If you see Mode 2 headers (02) and Subheaders (the extra 8 bytes we discussed earlier), it is uniquely a PlayStation format characteristic in the context of consoles. Sega and NEC stuck to the PC standard Mode 1.