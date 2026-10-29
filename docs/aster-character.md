# Aster the Astrocartographer

The approved design board is Aster’s visual reference. Use its large standing figure to compare the face, silhouette,
proportions, accessories and painterly finish of every adaptation.

![Approved Aster design board](https://github.com/user-attachments/assets/95033122-1aa8-415e-bcb4-11d1709e81cd)

## Character identity

| Feature | Keep consistent |
| --- | --- |
| Body | Warm golden star with rounded points and a soft, substantial silhouette |
| Face | Expressive teal eyes, gentle brows, warm cheeks and a small, knowing smile |
| Limbs | Distinct arms, hands, legs and bare feet, with the same proportions in every view |
| Clothing | Teal scarf and brown leather satchel with brass details |
| Brush | Long wooden handle, brass ferrule and generous paint-covered bristles; at least character height |
| Personality | Curious, warm, worldly and capable |

Star gold is `#E9B949`, deep teal `#236B70`, coral `#D9826B`, midnight `#182A3A`, and parchment `#F5EEDF`. Shading and
highlights stay within this palette’s visual relationships.

## Website source

Every website placement uses `server/public/brand/aster/aster.svg`. This is the editable web translation, with named
body, face, limb, scarf, satchel and brush groups. Section-specific CSS controls its size and placement.

The integration test verifies that all five homepage placements, including the shared footer, resolve to this one asset.
Keep one master when changing the web translation so each page receives the same character.

## Consistency checks

`server/public/brand/aster/CANON.toml` records the approved board, its exact attached PNG checksum, the five palette
colors, and the shared SVG checksum. The linked board is the visual reference; the checksum identifies the attachment
and does not assert that a re-encoded download has identical bytes.

Run `cargo test -p server --test aster_canon --test vesta_home`. These checks reject an unregistered variant, changed
SVG bytes, missing or duplicated limb/accessory groups, a changed reference or palette, and inconsistent homepage or
footer asset paths.

The current SVG is a **simplified translation that needs visual review**. Its checksum freezes the current drawing; it
does not certify that the drawing matches the painterly board. The manifest and tests preserve that distinction. Before
changing the checksum or approval status, compare the proposed asset against the board side by side and record the
review with the proposed artwork in the pull request. Match the face, silhouette, hands, feet, scarf, satchel, brush
scale and painterly finish. A new pose or animation must go through the same comparison.

## Poses and animation

Build poses from one layered character rig. A rig is the shared body, face, limbs and accessories with fixed attachment
points for movement. Change the pose, expression or camera angle while preserving the character’s identity.

Use the same rig for the website, Google Maps overlay and videos. The hands grip the same oversized brush, and its tip
follows the path being painted. Check proportions against the standing reference when Aster flies, sits or gestures.

A new adaptation is ready when its silhouette, face, limb proportions, scarf, satchel, brush and palette match the
reference side by side. Keep the original design board attached to every external illustration or animation brief.
