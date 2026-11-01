from __future__ import annotations

import re

_UNI = re.compile(r"^uni([0-9A-Fa-f]{4})(?:[0-9A-Fa-f]{4})*$")
_U = re.compile(r"^u([0-9A-Fa-f]{4,6})$")

_TABLE = """
space:20 exclam:21 quotedbl:22 numbersign:23 dollar:24 percent:25 ampersand:26 quotesingle:27
parenleft:28 parenright:29 asterisk:2A plus:2B comma:2C hyphen:2D period:2E slash:2F
zero:30 one:31 two:32 three:33 four:34 five:35 six:36 seven:37 eight:38 nine:39
colon:3A semicolon:3B less:3C equal:3D greater:3E question:3F at:40
bracketleft:5B backslash:5C bracketright:5D asciicircum:5E underscore:5F grave:60
braceleft:7B bar:7C braceright:7D asciitilde:7E
exclamdown:A1 cent:A2 sterling:A3 currency:A4 yen:A5 brokenbar:A6 section:A7 dieresis:A8
copyright:A9 ordfeminine:AA guillemotleft:AB logicalnot:AC registered:AE macron:AF degree:B0
plusminus:B1 twosuperior:B2 threesuperior:B3 acute:B4 mu:B5 paragraph:B6 periodcentered:B7
cedilla:B8 onesuperior:B9 ordmasculine:BA guillemotright:BB onequarter:BC onehalf:BD
threequarters:BE questiondown:BF Agrave:C0 Aacute:C1 Acircumflex:C2 Atilde:C3 Adieresis:C4
Aring:C5 AE:C6 Ccedilla:C7 Egrave:C8 Eacute:C9 Ecircumflex:CA Edieresis:CB Igrave:CC Iacute:CD
Icircumflex:CE Idieresis:CF Eth:D0 Ntilde:D1 Ograve:D2 Oacute:D3 Ocircumflex:D4 Otilde:D5
Odieresis:D6 multiply:D7 Oslash:D8 Ugrave:D9 Uacute:DA Ucircumflex:DB Udieresis:DC Yacute:DD
Thorn:DE germandbls:DF agrave:E0 aacute:E1 acircumflex:E2 atilde:E3 adieresis:E4 aring:E5 ae:E6
ccedilla:E7 egrave:E8 eacute:E9 ecircumflex:EA edieresis:EB igrave:EC iacute:ED icircumflex:EE
idieresis:EF eth:F0 ntilde:F1 ograve:F2 oacute:F3 ocircumflex:F4 otilde:F5 odieresis:F6
divide:F7 oslash:F8 ugrave:F9 uacute:FA ucircumflex:FB udieresis:FC yacute:FD thorn:FE
ydieresis:FF dotlessi:131 Lslash:141 lslash:142 OE:152 oe:153 Scaron:160 scaron:161 Ydieresis:178
Zcaron:17D zcaron:17E florin:192 circumflex:2C6 caron:2C7 breve:2D8 dotaccent:2D9 ring:2DA
ogonek:2DB tilde:2DC hungarumlaut:2DD endash:2013 emdash:2014 quoteleft:2018 quoteright:2019
quotesinglbase:201A quotedblleft:201C quotedblright:201D quotedblbase:201E dagger:2020
daggerdbl:2021 bullet:2022 ellipsis:2026 perthousand:2030 guilsinglleft:2039 guilsinglright:203A
fraction:2044 Euro:20AC trademark:2122 minus:2212 fi:FB01 fl:FB02 ff:FB00 ffi:FB03 ffl:FB04
"""
_TABLE += """
Alpha:391 Beta:392 Gamma:393 Delta:394 Epsilon:395 Zeta:396 Eta:397 Theta:398 Iota:399 Kappa:39A
Lambda:39B Mu:39C Nu:39D Xi:39E Omicron:39F Pi:3A0 Rho:3A1 Sigma:3A3 Tau:3A4 Upsilon:3A5 Phi:3A6
Chi:3A7 Psi:3A8 Omega:3A9 alpha:3B1 beta:3B2 gamma:3B3 delta:3B4 epsilon:3B5 zeta:3B6 eta:3B7
theta:3B8 iota:3B9 kappa:3BA lambda:3BB nu:3BD xi:3BE omicron:3BF pi:3C0 rho:3C1 sigma1:3C2
sigma:3C3 tau:3C4 upsilon:3C5 phi:3C6 chi:3C7 psi:3C8 omega:3C9 theta1:3D1 Upsilon1:3D2 phi1:3D5
omega1:3D6 epsilon1:3F5 Sigma1:3A3 Delta1:394 Gamma1:393 Theta1:398 Lambda1:39B Pi1:3A0 Xi1:39E
Omega1:3A9 Phi1:3A6 Psi1:3A8 varepsilon:3B5 varphi:3D5 vartheta:3D1 varpi:3D6 varrho:3F1 rho1:3F1
varsigma:3C2 aleph:2135 Ifraktur:2111 Rfraktur:211C weierstrass:2118 universal:2200 partialdiff:2202
existential:2203 emptyset:2205 gradient:2207 element:2208 notelement:2209 suchthat:220B
product:220F summation:2211 minus:2212 minusplus:2213 asteriskmath:2217 radical:221A
proportional:221D infinity:221E angle:2220 logicaland:2227 logicalor:2228 intersection:2229
union:222A integral:222B therefore:2234 similar:223C congruent:2245 approxequal:2248
notequal:2260 equivalence:2261 lessequal:2264 greaterequal:2265 propersubset:2282
propersuperset:2283 notsubset:2284 reflexsubset:2286 reflexsuperset:2287 circleplus:2295
circleminus:2296 circlemultiply:2297 circledivide:2298 circledot:2299 perpendicular:22A5
latticetop:22A4 dotmath:22C5 negationslash:338 arrowleft:2190 arrowup:2191 arrowright:2192
arrowdown:2193 arrowboth:2194 arrowupdn:2195 arrowdblleft:21D0 arrowdblup:21D1
arrowdblright:21D2 arrowdbldown:21D3 arrowdblboth:21D4 mapsto:21A6 arrowhookleft:21A9
arrowhookright:21AA arrownortheast:2197 arrowsoutheast:2198 arrownorthwest:2196
arrowsouthwest:2199 angbracketleft:27E8 angbracketright:27E9 angleleft:2329 angleright:232A
floorleft:230A floorright:230B ceilingleft:2308 ceilingright:2309 bardbl:2016 braceex:23AA
prime:2032 second:2033 circlecopyrt:24B8 club:2663 diamond:2662 heart:2661 spade:2660
lozenge:25CA openbullet:25E6 star:22C6 dagger:2020 daggerdbl:2021 section:A7 paragraph:B6
quotedblright:201D quotedblleft:201C sharp:266F flat:266D natural:266E wreathproduct:2240
turnstileleft:22A2 turnstileright:22A3 diamondmath:22C4 triangle:25B3 triangleinv:25BD
trianglesolid:25B2 arrowdblright1:21D2 lessmuch:226A greatermuch:226B precedesequal:2AAF
followsequal:2AB0 precedes:227A follows:227B similarequal:2243 emptyslot:2205 vector:20D7
equal1:2261 lscript:2113 hbar:210F imaginary:2111 real:211C ell:2113 owner:220B smallsetminus:2216
setminus:2216 backslash:5C bar:7C bullet:2022 periodcentered:B7 multiply:D7 divide:F7 plusminus:B1
dotlessj:237 openbullet1:2218 compose:2218 circ:2218 asciicircum:5E asciitilde:7E similar1:223C
"""

_NAMES: dict[str, str] = {
    name: chr(int(code, 16)) for name, code in (pair.split(":") for pair in _TABLE.split())
}
_NAMES.update({chr(c): chr(c) for c in range(0x21, 0x7F)})
_NAMES.update({"quotesingle": "'", "quoteleft": "‘", "quoteright": "’"})
_NAMES.update({"hyphenchar": "-", "sfthyphen": "­", "nbspace": " ", "visiblespace": "␣"})


def unicode_for(name: bytes) -> str | None:
    try:
        text = name.decode("ascii")
    except UnicodeDecodeError:
        return None
    text = text.split(".", 1)[0]
    if not text:
        return None
    hit = _NAMES.get(text)
    if hit is not None:
        return hit
    match = _UNI.match(text)
    if match:
        return "".join(chr(int(text[i : i + 4], 16)) for i in range(3, len(text), 4))
    match = _U.match(text)
    if match:
        code = int(match.group(1), 16)
        return chr(code) if code <= 0x10FFFF and not 0xD800 <= code <= 0xDFFF else None
    return None
