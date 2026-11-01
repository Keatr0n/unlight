//! What a soft proof hands the view path. The colour management behind it stays on the CPU, so
//! the engine only ever sees a table.

/// The working-space encoding of a table's axes: linear light raised to `1 / PROOF_GAMMA`, which
/// puts grid points where the eye can tell them apart.
pub const PROOF_GAMMA: f32 = 2.2;

/// How colours outside a target's gamut are brought in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ProofIntent {
    /// The whole gamut is compressed to fit, if the profile has tables for that.
    Perceptual,
    /// In-gamut colours stay put and the rest clip to the gamut's edge.
    #[default]
    Relative,
}

/// A profile baked to a cube over working-space colour.
#[derive(Clone, Debug)]
pub struct ProofLut {
    /// Tells tables apart. Two with the same id hold the same numbers.
    pub id: u64,
    /// Grid points along each axis.
    pub size: u32,
    /// Red changes fastest, then green, then blue. Each entry is the sRGB-encoded colour to
    /// show, then how far outside the gamut the colour is, from 0 to 1.
    pub entries: Vec<[f32; 4]>,
}

impl PartialEq for ProofLut {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}
