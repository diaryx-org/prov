---
title: Author's notes
part_of: '[prov](/README.md)'
author: adammharris
---

I have complex feelings about `prov`.
In some ways, it is my magnum opus.
It is the purest expression yet of my philosophy about how information should be organized.
But it yet falls short in many ways,
and it is so ambitious that doing it "complete" and "right" may be impossible.

I want pluggable interfaces for authoring/reading:
- specific configuration formats (adding to JSON, YAML, TOML; currently done via `fig`)
- specific document formats (adding to Markdown, HTML, Djot; currently done via `twig`)
- filesystems (already somewhat in place with the Filesystem trait)
- ID generation (currently via moid)

My motivation for this is so that all `prov` contains is logic specific to preservation, provenance, fixity, hierarchy,
and all other things necessary to make sure a digital archive lasts forever.

That is my ultimate, most ambitious goal: making a digital archive last forever.
Like, literally until Jesus comes back.
(So I have something to show Him!)

Obviously no code library can literally guarantee "forever." No one can.
But I draw inspiration from the story of Mormon and Moroni,
who engraved the history of their civilization in plates of gold.
Gold does not rust, or fade, or tarnish.
The gold on the sarcophagus of King Tut is just as bright as it was when he was buried.
Similarly, the honey stored in their tombs was still edible.

I want my writing to be engraved in gold.
I want the taste of my writing to be preserved like honey.

As impossible (and as prideful) as that is,
I can't help wanting that.
You could say it is a part of my raison d'être.