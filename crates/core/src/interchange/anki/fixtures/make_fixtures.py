"""Makes the Anki test packages of Rappel's Anki import (ADR 0061).

Run it with Anki's own Python library, on a FRESH scratch folder (see
README.md for the exact command). It never opens an existing collection or
an Anki profile: the collection is created in the scratch folder, and HOME
points there too.

    make_fixtures.py <scratch dir> <out dir> <media dir>

Writes into <out dir>:
  current.apkg   the current format (meta version 3, collection.anki21b and
                 media compressed with zstd, a protobuf media list)
  legacy.apkg    the legacy format (collection.anki21, a JSON media list)
  anki2.apkg     derived from legacy.apkg: its collection.anki21 stored as
                 collection.anki2 (both are schema 11), as Anki 2.0 wrote
  current.colpkg a collection package in the current format

All the content is synthetic.
"""

import os
import shutil
import sys
import zipfile

from anki.collection import Collection, ExportAnkiPackageOptions

scratch, out, media_dir = sys.argv[1], sys.argv[2], sys.argv[3]
path = os.path.join(scratch, "fixtures.anki2")
for leftover in ("fixtures.anki2", "fixtures.media"):
    p = os.path.join(scratch, leftover)
    if os.path.isdir(p):
        shutil.rmtree(p)
    elif os.path.exists(p):
        os.remove(p)
col = Collection(path)

for name in ("local.png", "photo.jpg", "clip.mp3", "pic.svg", "unused.png"):
    added = col.media.add_file(os.path.join(media_dir, name))
    assert added == name, added
# gone.png is named by a card but never added: a missing file.


def note(model_name, fields, deck, tags=()):
    model = col.models.by_name(model_name)
    n = col.new_note(model)
    for key, value in fields.items():
        n[key] = value
    n.tags = list(tags)
    col.add_note(n, col.decks.id(deck))
    return n


# Note types: the stock ones, and a custom one.
mm = col.models
vocab = mm.new("Vocab")
for field in ("Word", "Meaning", "Example", "Note"):
    mm.add_field(vocab, mm.new_field(field))
t1 = mm.new_template("Recognition")
t1["qfmt"] = "{{Word}}{{#Example}}<div class=ex>{{Example}}</div>{{/Example}}"
t1["afmt"] = (
    "{{FrontSide}}\n\n<hr id=answer>\n\n{{Meaning}}"
    "{{^Note}}<i>no note</i>{{/Note}}{{#Note}} {{text:Note}}{{/Note}}"
    " {{myaddon:Word}}"
)
mm.add_template(vocab, t1)
t2 = mm.new_template("Info")
t2["qfmt"] = "{{Meaning}} ({{Card}}, {{Type}})"
t2["afmt"] = "{{Word}}<br>Deck {{Deck}}, subdeck {{Subdeck}}, tags {{Tags}}"
mm.add_template(vocab, t2)
mm.add(vocab)

basic = note(
    "Basic",
    {
        "Front": "Bonjour <b>bold</b> <i>italic</i> <u>under</u> &amp; &lt;x&gt; caf&eacute; &#233;&#x1F600;&nbsp;!",
        "Back": "<ul><li>one</li><li>two <b>2</b></li></ul><ol><li>first</li></ol>"
        "line<br>break<div>a div</div><span style=\"color: red\">red</span>",
    },
    "Default",
    ["geo::europe", "日本語", "über"],
)
reversed_note = note(
    "Basic (and reversed card)",
    {"Front": "chat", "Back": "cat"},
    "Lang::Français::Verbes",
    ["marked"],
)
note(
    "Basic (optional reversed card)",
    {"Front": "chien", "Back": "dog", "Add Reverse": "y"},
    "Lang::Français::Verbes",
)
note(
    "Basic (optional reversed card)",
    {"Front": "oiseau", "Back": "bird", "Add Reverse": ""},
    "Lang::Français::Verbes",
)
# Its reverse card was made, then the field emptied: the card stays, with
# an empty front (Anki's Empty Cards would remove it).
emptied = note(
    "Basic (optional reversed card)",
    {"Front": "poisson", "Back": "fish", "Add Reverse": "y"},
    "Lang::Français::Verbes",
)
emptied["Add Reverse"] = ""
col.update_note(emptied)
assert len(emptied.card_ids()) == 2
note(
    "Basic (type in the answer)",
    {"Front": "Capital of Italy?", "Back": "Rome"},
    "Lang",
)
cloze = note(
    "Cloze",
    {
        "Text": "{{c1::Paris}} is the capital of {{c2::France::country}}; the {{c1::Seine}} runs through it.",
        "Back Extra": "Geography",
    },
    "Science::Geo",
    ["leech"],
)
note(
    "Vocab",
    {"Word": "manger", "Meaning": "to eat", "Example": "Je mange.", "Note": "<b>irregular</b>"},
    "Lang::Français::Verbes",
)
note(
    "Vocab",
    {"Word": "boire", "Meaning": "to drink", "Example": "", "Note": ""},
    "Lang::Français::Verbes",
)
html = note(
    "Basic",
    {
        "Front": "Script <script>alert(1)</script>gone, <a href=\"https://example.org/page\">a link</a>,"
        " <img src=\"local.png\"> and <img src=\"https://example.org/remote.png\">",
        "Back": "[sound:clip.mp3] <img src=\"pic.svg\"> <img src=\"photo.jpg\" alt=\"a photo\">"
        " <img src=\"gone.png\"> Costs $5 and 3*4*5_6 # not a heading",
    },
    "Science",
)
math = note(
    "Basic",
    {
        "Front": "Inline \\(x^2 &lt; y\\) and display \\[\\int_0^1 x\\,dx\\]",
        "Back": "<pre><code>fn main() {\n    println!(\"*hi*\");\n}</code></pre>"
        "Use <code>a_b</code> here.",
    },
    "Science",
)

# Review history, with Anki's own scheduler.
def answer(card_id, ratings):
    for rating in ratings:
        card = col.get_card(card_id)
        card.start_timer()
        card.timer_started -= 7  # seven seconds to answer
        col.sched.answerCard(card, rating)


basic_card = basic.card_ids()[0]
# Learn (Again, Good, Good), review (Good), relearn (Again on a review,
# then Good), review again (Hard, Easy).
answer(basic_card, [1, 3, 3, 3, 1, 3, 2, 4])
front_card, back_card = reversed_note.card_ids()
answer(front_card, [3, 3, 3])
# Reviewed, then rescheduled by hand ("Set due date", ease 0, manual).
answer(back_card, [4])
col.sched.set_due_date([back_card], "5")
cloze_cards = cloze.card_ids()
# Reviewed, then forgotten (Reset), which Anki logs as manual, ease 0.
answer(cloze_cards[0], [3, 3])
col.sched.schedule_cards_as_new([cloze_cards[0]])
answer(cloze_cards[1], [3])
math_card = math.card_ids()[0]
answer(math_card, [3])
col.sched.suspend_cards([math_card])
col.sched.bury_cards([html.card_ids()[0]])

# Filtered decks, as Anki logs them: a review card answered before it is
# due in a filtered deck that reschedules (type 3 with an ease factor, kept
# as a review), and a new card answered in one that does not (a preview:
# type 3 with factor 0, left out). Both go home afterwards.
early = col.decks.new_filtered("Early")
deck = col.decks.get(early)
deck["terms"] = [["cid:%d" % front_card, 10, 0]]
deck["resched"] = True
col.decks.save(deck)
col.sched.rebuild_filtered_deck(early)
answer(front_card, [3])
col.sched.empty_filtered_deck(early)
preview = col.decks.new_filtered("Preview")
deck = col.decks.get(preview)
capital = col.find_cards('"Front:Capital of Italy?"')[0]
deck["terms"] = [["cid:%d" % capital, 10, 0]]
deck["resched"] = False
col.decks.save(deck)
col.sched.rebuild_filtered_deck(preview)
answer(capital, [3])
col.sched.empty_filtered_deck(preview)
for removed in (early, preview):
    col.decks.remove([removed])
kinds = col.db.all("select type, factor from revlog where type = 3")
assert sorted(kinds) == [[3, 0], [3, 2500]], kinds

# Spread the log over the past days: one entry every 20 hours, in the order
# they were made (the scheduler ran within a second).
rows = col.db.all("select id from revlog order by id")
start = rows[-1][0] - len(rows) * 20 * 3600 * 1000
for i, (old,) in enumerate(rows):
    col.db.execute("update revlog set id = ? where id = ?", -(i + 1), old)
for i in range(len(rows)):
    col.db.execute("update revlog set id = ? where id = ?", start + i * 20 * 3600 * 1000, -(i + 1))

# A filtered deck that pulls the Science cards out of their home deck.
fid = col.decks.new_filtered("Cram")
deck = col.decks.get(fid)
deck["terms"] = [["deck:Science", 100, 0]]
col.decks.save(deck)
col.sched.rebuild_filtered_deck(fid)
assert col.db.scalar("select count() from cards where odid != 0") > 0

os.makedirs(out, exist_ok=True)
for legacy, name in ((False, "current.apkg"), (True, "legacy.apkg")):
    col.export_anki_package(
        out_path=os.path.join(out, name),
        options=ExportAnkiPackageOptions(
            with_scheduling=True, with_deck_configs=True, with_media=True, legacy=legacy
        ),
        limit=None,
    )
col.export_collection_package(os.path.join(out, "current.colpkg"), include_media=True, legacy=False)

# Anki 2.0's packages hold the real collection in collection.anki2 (schema
# 11, as collection.anki21 is) and have no `meta`.
with zipfile.ZipFile(os.path.join(out, "legacy.apkg")) as src, zipfile.ZipFile(
    os.path.join(out, "anki2.apkg"), "w", zipfile.ZIP_DEFLATED
) as dst:
    for info in src.infolist():
        if info.filename in ("meta", "collection.anki2"):
            continue
        data = src.read(info.filename)
        name = "collection.anki2" if info.filename == "collection.anki21" else info.filename
        dst.writestr(name, data)
print("fixtures written to", out)
