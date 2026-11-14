import io
p = "LICENSE-APACHE"
s = io.open(p, encoding="utf-8").read()
s = s.replace("   Copyright [yyyy] [name of copyright owner]", "   Copyright 2026 jcar01")
io.open(p, "w", encoding="utf-8", newline="\n").write(s)
print("appendix:", "Copyright 2026 jcar01" in s)

p = "Cargo.toml"
s = io.open(p, encoding="utf-8").read()
if "license" not in s:
    s = s.replace('edition = "2024"',
                  'edition = "2024"\nlicense = "MIT OR Apache-2.0"\nrepository = "https://github.com/jcar01/solidcraft-plugin-iso-sheet"\ndescription = "Isometric manufacturing sheet plugin for SolidCraft"')
io.open(p, "w", encoding="utf-8", newline="\n").write(s)
print(s.split("\n")[:8])

p = "README.md"
s = io.open(p, encoding="utf-8").read()
s = s.rstrip() + """

## License

Dual-licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in
this project by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without
any additional terms or conditions.
"""
io.open(p, "w", encoding="utf-8", newline="\n").write(s)
print("readme license section added")
