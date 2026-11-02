import sys
TEXT = ["The harbor was quiet before dawn and the boats waited in a row.",
        "A cold wind moved over the water while the town slept behind the pier.",
        "The old sailor watched from the shore as the ships prepared to leave.",
        "Gulls circled above the masts and called to each other in the gray light.",
        "A young deckhand carried coils of rope down the narrow wooden steps.",
        "The baker on the corner opened his shutters and set out warm bread.",
        "Fishermen checked their nets for tears and folded them into wide baskets.",
        "Far out at sea a lighthouse blinked its slow and steady signal.",
        "The tide was turning, and the water lapped against the stone wall.",
        "One by one the boats untied their lines and drifted toward open water.",
        "Children ran along the beach, chasing the foam as it slid back.",
        "By the time the sun cleared the hills, the harbor stood almost empty.",
        "Only the old sailor remained, counting the sails until they were gone."]
ops = "BT /F1 14 Tf 72 700 Td 22 TL " + " ".join(f"({t}) Tj T*" for t in TEXT) + " ET"
objs = ["<</Type/Catalog/Pages 2 0 R>>", "<</Type/Pages/Kids[3 0 R]/Count 1>>",
        "<</Type/Page/Parent 2 0 R/MediaBox[0 0 612 792]/Resources<</Font<</F1 5 0 R>>>>/Contents 4 0 R>>",
        f"<</Length {len(ops)}>>\nstream\n{ops}\nendstream", "<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>"]
out = b"%PDF-1.4\n"; offs = []
for i, o in enumerate(objs, 1):
    offs.append(len(out)); out += f"{i} 0 obj\n{o}\nendobj\n".encode()
x = len(out)
out += f"xref\n0 {len(objs)+1}\n0000000000 65535 f \n".encode() + "".join(f"{o:010d} 00000 n \n" for o in offs).encode()
out += f"trailer\n<</Size {len(objs)+1}/Root 1 0 R>>\nstartxref\n{x}\n%%EOF\n".encode()
open(sys.argv[1], "wb").write(out)
