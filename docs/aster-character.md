# Aster the Astrocartographer

The user-approved design board is Aster’s canonical artwork. Every website pose frames its original pixels, preserving
the eyes, rounded form, painted texture, hands, feet and oversized brush.

![Approved Aster design board](https://github.com/user-attachments/assets/95033122-1aa8-415e-bcb4-11d1709e81cd)

## Character identity

| Feature | Keep consistent |
| --- | --- |
| Body | Warm golden star with rounded points and a soft, substantial silhouette |
| Face | Expressive teal eyes, gentle brows, warm cheeks and a small, knowing smile |
| Limbs | Distinct arms, hands, legs and bare feet, with the proportions in the board |
| Clothing | Teal scarf and brown leather satchel with brass details |
| Brush | Long wooden handle, brass ferrule and generous paint-covered bristles; at least character height |
| Personality | Curious, warm, worldly and capable |

Star gold is `#E9B949`, deep teal `#236B70`, coral `#D9826B`, midnight `#182A3A`, and parchment `#F5EEDF`.

## Website source

The original PNG lives in the public asset lane at `img/vesta-aster/aster-canon-7d379560.png`. Its bytes stay outside
Git. `views::assets::ASTER_BOARD_KEY` supplies the shared object key to the home page and footer through the asset
resolver. The public asset inventory includes the key for restoration, publication checks and orphan protection.

`webapp/src/components/aster.rs` renders inline SVG viewports for the standing, Explore, Illustrate and Guide figures.
Each viewport displays the same original image. It crops the surrounding board without reconstructing or recoloring the
character. SVG overlays and future map lines can remain vectors around the original illustration.

## Consistency checks

`server/public/brand/aster/CANON.toml` pins the approved attachment’s SHA-256, dimensions, source URL, object key and
palette. Run the ordinary contract and placement checks with:

```bash
cargo test -p server --test aster_canon --test vesta_home
```

After restoring the public PNG, verify its actual bytes and dimensions too:

```bash
cargo test -p server --test aster_canon -- --include-ignored
```

The byte check is explicitly ignored in an ordinary offline checkout because public raster assets are not committed. A
passing metadata check alone does not verify an absent PNG. The rendered integration test checks all five home-page
placements and the services footer against the one resolved board URL. The component test pins the four viewports and
checks that each frame stays within the board.

## Poses and animation

The board contains the approved standing, Explore, Illustrate and Guide poses. Use them for static website placements. A
production animation rig requires separately prepared layers and visual comparison against this original artwork. Any
new pose must preserve the eyes, face, silhouette, limbs, scarf, satchel, brush scale and painterly finish. Keep the
canonical board attached to every external illustration or animation brief.
