// Use mimalloc as the global allocator for faster memory allocation
// (reduces ~17% CPU time spent in glibc malloc/free)
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

// Rust implementation of ctags data structures (incremental migration from C)
pub mod ctags_rs;

use std::fs::File;
use std::io::{self, BufWriter, Write};
use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};
use std::sync::Arc;
use parking_lot::Mutex;  // Faster than std::sync::Mutex for uncontended locks
use std::sync::atomic::{AtomicU64, AtomicBool, Ordering};
use once_cell::sync::Lazy;
use std::cell::RefCell;
use std::thread_local;
use std::time::Instant;

// ============================================================================
// String Interner - Optimization #1
// Reduces string allocation overhead by storing strings once and using u32 IDs
// ============================================================================

/// Interned string ID - cheap to copy, compare, and hash
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[repr(transparent)]
pub struct InternId(u32);

/// String interner for deduplicating PU keys and dependency names
/// Stores strings in a Vec and maps them to u32 indices for O(1) equality/hash
pub struct StringInterner {
    /// String storage - index is the InternId
    strings: Vec<String>,
    /// Reverse lookup: string -> InternId (for interning)
    lookup: FxHashMap<String, InternId>,
}

impl StringInterner {
    /// Create a new interner with pre-allocated capacity
    pub fn with_capacity(capacity: usize) -> Self {
        StringInterner {
            strings: Vec::with_capacity(capacity),
            lookup: FxHashMap::with_capacity_and_hasher(capacity, Default::default()),
        }
    }

    /// Intern a string, returning its ID
    #[inline]
    pub fn intern(&mut self, s: &str) -> InternId {
        if let Some(&id) = self.lookup.get(s) {
            return id;
        }
        let id = InternId(self.strings.len() as u32);
        self.strings.push(s.to_string());
        self.lookup.insert(s.to_string(), id);
        id
    }

    /// Intern an owned string, avoiding clone if possible
    #[inline]
    pub fn intern_owned(&mut self, s: String) -> InternId {
        if let Some(&id) = self.lookup.get(&s) {
            return id;
        }
        let id = InternId(self.strings.len() as u32);
        self.lookup.insert(s.clone(), id);
        self.strings.push(s);
        id
    }

    /// Get string for an ID (for output)
    #[inline]
    pub fn get(&self, id: InternId) -> &str {
        &self.strings[id.0 as usize]
    }

    /// Get number of interned strings
    #[inline]
    pub fn len(&self) -> usize {
        self.strings.len()
    }

    /// Check if empty
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.strings.is_empty()
    }

    /// Get ID for a string if it exists (without interning)
    #[inline]
    pub fn get_id(&self, s: &str) -> Option<InternId> {
        self.lookup.get(s).copied()
    }
}

// ============================================================================
// Global Interner - Thread-safe read-only access after initialization
// ============================================================================

/// Global interner that is built once and provides fast read-only access
/// Uses Arc for thread-safe sharing during parallel processing
pub struct GlobalInterner {
    /// The underlying string interner (read-only after build)
    interner: StringInterner,
    /// Reverse lookup from string hash to ID for O(1) access
    /// Uses FxHashMap for fast hashing
    _str_to_id: FxHashMap<u64, InternId>,
}

impl GlobalInterner {
    /// Build a global interner from all PU keys
    pub fn build(pu_order: &[String], tags: &FxHashMap<String, Vec<String>>) -> Self {
        // Estimate capacity: pu_order + all tag values
        let estimated_capacity = pu_order.len() + tags.values().map(|v| v.len()).sum::<usize>();
        let mut interner = StringInterner::with_capacity(estimated_capacity);

        // Intern all PU keys
        for key in pu_order {
            interner.intern(key);
        }

        // Intern all tag unit keys
        for units in tags.values() {
            for unit in units {
                interner.intern(unit);
            }
        }

        // Build hash-based reverse lookup for O(1) string -> ID
        use std::hash::{Hash, Hasher};
        let mut str_to_id = FxHashMap::with_capacity_and_hasher(interner.len(), Default::default());
        for i in 0..interner.len() {
            let id = InternId(i as u32);
            let s = interner.get(id);
            let mut hasher = rustc_hash::FxHasher::default();
            s.hash(&mut hasher);
            str_to_id.insert(hasher.finish(), id);
        }

        GlobalInterner { interner, _str_to_id: str_to_id }
    }

    /// Get ID for a string (O(1) lookup)
    #[inline]
    pub fn get_id(&self, s: &str) -> Option<InternId> {
        self.interner.get_id(s)
    }

    /// Get string for an ID (O(1) lookup)
    #[inline]
    pub fn get_str(&self, id: InternId) -> &str {
        self.interner.get(id)
    }

    /// Intern a string if not already present, or return existing ID
    /// Note: This requires the interner to be mutable, so only use during build phase
    #[inline]
    pub fn intern(&mut self, s: &str) -> InternId {
        self.interner.intern(s)
    }

    /// Get number of interned strings
    #[inline]
    pub fn len(&self) -> usize {
        self.interner.len()
    }
}

// ============================================================================
// Interned Necessary Set - Uses InternId instead of String for O(1) operations
// ============================================================================

/// A set of necessary PU keys using InternId for fast operations
/// Eliminates string hashing and comparison overhead
pub struct InternedNecessarySet {
    /// Set of interned IDs
    ids: FxHashSet<InternId>,
}

impl InternedNecessarySet {
    /// Create a new empty set with estimated capacity
    #[inline]
    pub fn with_capacity(capacity: usize) -> Self {
        InternedNecessarySet {
            ids: FxHashSet::with_capacity_and_hasher(capacity, Default::default()),
        }
    }

    /// Insert an ID, returns true if newly inserted
    #[inline]
    pub fn insert(&mut self, id: InternId) -> bool {
        self.ids.insert(id)
    }

    /// Check if ID is in the set
    #[inline]
    pub fn contains(&self, id: InternId) -> bool {
        self.ids.contains(&id)
    }

    /// Get number of elements
    #[inline]
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    /// Check if empty
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// Iterate over IDs
    #[inline]
    pub fn iter(&self) -> impl Iterator<Item = InternId> + '_ {
        self.ids.iter().copied()
    }

    /// Convert to FxHashSet<String> for compatibility with existing code
    pub fn to_string_set(&self, interner: &GlobalInterner) -> FxHashSet<String> {
        self.ids.iter()
            .map(|&id| interner.get_str(id).to_string())
            .collect()
    }

    /// Extend from another set
    #[inline]
    pub fn extend(&mut self, other: impl IntoIterator<Item = InternId>) {
        self.ids.extend(other);
    }
}

// ============================================================================
// Interned Transitive Dependencies - Uses InternId for fast lookups
// ============================================================================

/// Pre-computed transitive dependencies using InternId for fast access
/// This eliminates string hashing overhead in the hot path
pub struct InternedTransitiveDeps {
    /// For each InternId, the set of all transitive dependency IDs
    deps: Vec<FxHashSet<InternId>>,
}

impl InternedTransitiveDeps {
    /// Build from TransitiveDeps and GlobalInterner
    pub fn from_transitive_deps(
        trans_deps: &TransitiveDeps,
        interner: &GlobalInterner,
    ) -> Self {
        let num_ids = interner.len();
        let mut deps: Vec<FxHashSet<InternId>> = vec![FxHashSet::default(); num_ids];

        for (key, dep_set) in &trans_deps.deps {
            if let Some(key_id) = interner.get_id(key) {
                let interned_deps: FxHashSet<InternId> = dep_set.iter()
                    .filter_map(|dep| interner.get_id(dep))
                    .collect();
                deps[key_id.0 as usize] = interned_deps;
            }
        }

        InternedTransitiveDeps { deps }
    }

    /// Get transitive dependencies for an ID
    #[inline]
    pub fn get(&self, id: InternId) -> Option<&FxHashSet<InternId>> {
        self.deps.get(id.0 as usize)
    }
}

// ============================================================================
// Interned Position Index - Uses InternId for O(1) valid_keys checks
// ============================================================================

/// Pre-computed position index using InternId
pub struct InternedPositionIndex {
    /// InternId -> position in pu_order
    id_to_pos: Vec<Option<usize>>,
}

impl InternedPositionIndex {
    /// Build from PositionIndex and GlobalInterner
    pub fn from_position_index(
        pos_index: &PositionIndex,
        interner: &GlobalInterner,
    ) -> Self {
        let num_ids = interner.len();
        let mut id_to_pos: Vec<Option<usize>> = vec![None; num_ids];

        for (key, &pos) in &pos_index.key_to_pos {
            if let Some(id) = interner.get_id(key) {
                id_to_pos[id.0 as usize] = Some(pos);
            }
        }

        InternedPositionIndex { id_to_pos }
    }

    /// Check if an ID is valid (position <= max_pos)
    #[inline]
    pub fn is_valid(&self, id: InternId, max_pos: usize) -> bool {
        self.id_to_pos.get(id.0 as usize)
            .and_then(|&pos| pos)
            .map(|pos| pos <= max_pos)
            .unwrap_or(false)
    }

    /// Get position for an ID
    #[inline]
    pub fn get_pos(&self, id: InternId) -> Option<usize> {
        self.id_to_pos.get(id.0 as usize).and_then(|&pos| pos)
    }
}

// ============================================================================
// Interned Dependency Helper - Uses InternId for fast transitive lookups
// ============================================================================

/// Collect transitive dependencies using interned IDs for fast lookups
/// This is the optimized hot path for dependency resolution
/// Returns the keys to add to the necessary set
#[inline]
pub fn collect_transitive_deps_interned(
    keys: &[&str],
    max_pos: usize,
    interner: &GlobalInterner,
    trans_deps: &InternedTransitiveDeps,
    pos_index: &InternedPositionIndex,
    already_in: &FxHashSet<String>,
) -> Vec<String> {
    let mut result = Vec::new();

    for &key in keys {
        // Look up InternId for this key
        if let Some(key_id) = interner.get_id(key) {
            // Get transitive deps using InternId (O(1) lookup)
            if let Some(deps) = trans_deps.get(key_id) {
                // Filter by position and convert back to strings
                for &dep_id in deps.iter() {
                    if pos_index.is_valid(dep_id, max_pos) {
                        let dep_str = interner.get_str(dep_id);
                        if !already_in.contains(dep_str) {
                            result.push(dep_str.to_string());
                        }
                    }
                }
            }
        }
    }

    result
}

/// Fast fixpoint loop using interned IDs
/// Returns all new dependencies found after resolving transitive deps
/// OPTIMIZATION: Reuses buffer for to_process to avoid repeated allocations
#[inline]
pub fn fixpoint_transitive_deps_interned(
    necessary: &mut FxHashSet<String>,
    initial_keys: &[String],
    max_pos: usize,
    interner: &GlobalInterner,
    trans_deps: &InternedTransitiveDeps,
    pos_index: &InternedPositionIndex,
) {
    // Build a set of InternIds for items that have been processed
    let mut processed_ids: FxHashSet<InternId> = FxHashSet::with_capacity_and_hasher(initial_keys.len() * 2, Default::default());

    // Convert initial keys to InternIds and mark as processed
    for key in initial_keys {
        if let Some(id) = interner.get_id(key) {
            processed_ids.insert(id);
        }
    }

    // Build a set of InternIds for current necessary items
    let mut necessary_ids: FxHashSet<InternId> = FxHashSet::with_capacity_and_hasher(necessary.len() * 2, Default::default());
    for key in necessary.iter() {
        if let Some(id) = interner.get_id(key) {
            necessary_ids.insert(id);
        }
    }

    // OPTIMIZATION: Reuse buffer for to_process instead of allocating each iteration
    let mut to_process: Vec<InternId> = Vec::with_capacity(necessary.len());

    // Fixpoint loop using InternIds
    loop {
        // Clear and refill the reusable buffer
        to_process.clear();
        to_process.extend(
            necessary_ids.iter()
                .filter(|id| !processed_ids.contains(id))
                .copied()
        );

        if to_process.is_empty() {
            break;
        }

        for &id in &to_process {
            processed_ids.insert(id);

            // Get transitive deps using InternId
            if let Some(deps) = trans_deps.get(id) {
                for &dep_id in deps.iter() {
                    // Filter by position
                    if pos_index.is_valid(dep_id, max_pos) {
                        necessary_ids.insert(dep_id);
                    }
                }
            }
        }
    }

    // Convert new InternIds back to strings and add to necessary
    for id in necessary_ids.iter() {
        let key = interner.get_str(*id);
        necessary.insert(key.to_string());
    }
}

// ============================================================================
// Position Index Map - Optimization #3
// Pre-computes pu_key -> position for O(1) valid_keys filtering
// ============================================================================

/// Pre-computed position index for O(1) valid_keys checks
/// Instead of building HashSet<String> per PU, use position comparisons
pub struct PositionIndex {
    /// pu_key -> position in pu_order (None if not in pu_order)
    pub key_to_pos: FxHashMap<String, usize>,
}

impl PositionIndex {
    /// Build position index from pu_order
    pub fn from_pu_order(pu_order: &[String]) -> Self {
        let mut key_to_pos = FxHashMap::with_capacity_and_hasher(pu_order.len(), Default::default());
        for (i, key) in pu_order.iter().enumerate() {
            key_to_pos.insert(key.clone(), i);
        }
        PositionIndex { key_to_pos }
    }

    /// Check if a key is valid (position < max_pos)
    #[inline]
    pub fn is_valid(&self, key: &str, max_pos: usize) -> bool {
        self.key_to_pos.get(key).map(|&pos| pos <= max_pos).unwrap_or(false)
    }

    /// Get position for a key
    #[inline]
    pub fn get_pos(&self, key: &str) -> Option<usize> {
        self.key_to_pos.get(key).copied()
    }

    /// Check if a key exists in the position index (i.e., is in pu_order)
    /// OPTIMIZATION: Use this instead of building HashSet from pu_order.iter()
    #[inline]
    pub fn contains(&self, key: &str) -> bool {
        self.key_to_pos.contains_key(key)
    }
}

// ============================================================================
// Split Mode Configuration
// Consolidates all split/non-split mode settings into a single struct
// ============================================================================

/// Configuration for split mode vs non-split mode processing
/// This struct consolidates all environment variable settings and mode-specific options
#[derive(Clone, Debug)]
pub struct SplitConfig {
    /// Whether split mode is enabled (SPLIT env var)
    pub is_split: bool,
    /// Whether common header generation is enabled (USE_COMMON_HEADER env var, requires is_split)
    pub use_common_header: bool,
    /// Whether chunked split mode is enabled (SPLIT_COUNT env var, requires is_split)
    pub is_chunked: bool,
    /// Optional filter for specific PU UIDs (PU_FILTER env var, requires is_split)
    pub pu_filter: Option<FxHashSet<usize>>,
    /// Starting PU UID to skip earlier PUs (START_PU env var, requires is_split)
    pub start_pu: usize,
    /// PCH mode: emit delta PU files + full common header for precompiled headers (PRECC_PCH env var)
    pub use_pch: bool,
}

impl SplitConfig {
    /// Create a SplitConfig by reading environment variables
    pub fn from_env() -> Self {
        let is_split = std::env::var("SPLIT").is_ok();

        if !is_split {
            // Non-split mode: all options disabled
            return SplitConfig {
                is_split: false,
                use_common_header: false,
                is_chunked: false,
                pu_filter: None,
                start_pu: 0,
                use_pch: false,
            };
        }

        // Split mode: read additional configuration
        let use_common_header = std::env::var("USE_COMMON_HEADER").is_ok();
        let use_pch = std::env::var("PRECC_PCH").is_ok();
        let is_chunked = std::env::var("SPLIT_COUNT").is_ok();

        let pu_filter: Option<FxHashSet<usize>> = std::env::var("PU_FILTER")
            .ok()
            .map(|s| {
                s.split(',')
                    .filter_map(|n| n.trim().parse::<usize>().ok())
                    .collect()
            });

        let start_pu: usize = std::env::var("START_PU")
            .ok()
            .and_then(|s| s.trim().parse::<usize>().ok())
            .unwrap_or(0);

        SplitConfig {
            is_split,
            use_common_header,
            is_chunked,
            pu_filter,
            start_pu,
            use_pch,
        }
    }

    /// Log configuration if any special filters are active
    pub fn log_filters(&self) {
        if let Some(ref filter) = self.pu_filter {
            eprintln!("[PU_FILTER] Generating only {} PUs: {:?}", filter.len(), filter);
        }
        if self.start_pu > 0 {
            eprintln!("[START_PU] Skipping PUs with UID < {}", self.start_pu);
        }
    }

    /// Check if a UID should be processed based on filters
    #[inline]
    pub fn should_process_uid(&self, uid: usize) -> bool {
        if uid < self.start_pu {
            return false;
        }
        if let Some(ref filter) = self.pu_filter {
            return filter.contains(&uid);
        }
        true
    }
}

// ============================================================================
// Pre-computed Structures
// Consolidates all pre-computed data structures built once before parallel processing
// ============================================================================

/// Pre-computed structures built once before parallel/sequential processing
/// This eliminates duplicate initialization code between split and non-split modes
pub struct PrecomputedStructures {
    /// Shared maps for typedef, prototype, function, struct, union lookups
    pub shared_maps: SharedMaps,
    /// Parsed tags for O(1) pu_key lookup
    pub parsed_tags: ParsedTagsMap,
    /// Transitive dependencies for each PU
    pub transitive_deps: TransitiveDeps,
    /// Position index for O(1) valid_keys checks
    pub position_index: PositionIndex,
    /// Project types for filtering system typedefs
    pub project_types: ProjectTypes,
    /// Pre-tokenized identifiers for each PU code block (Optimization #1)
    /// Eliminates redundant tokenization in print_necessary_units
    pub code_identifiers: CodeIdentifiers,
    /// Global string interner for InternId-based operations (Optimization #2)
    pub interner: GlobalInterner,
    /// Interned transitive dependencies for fast lookups
    pub interned_trans_deps: InternedTransitiveDeps,
    /// Interned position index for fast valid_keys checks
    pub interned_pos_index: InternedPositionIndex,
}

/// Pre-computed identifier sets for all PU code blocks
/// This eliminates repeated tokenization during per-PU processing
pub struct CodeIdentifiers {
    /// pu_key -> set of C identifiers found in that code block
    identifiers: FxHashMap<String, FxHashSet<String>>,
    /// pu_key -> set of extern-declared function names (Optimization #3)
    extern_funcs: FxHashMap<String, FxHashSet<String>>,
}

impl CodeIdentifiers {
    /// Build identifier sets for all PU code blocks in parallel
    pub fn build(pu: &FxHashMap<String, String>) -> Self {
        let mut identifiers = FxHashMap::with_capacity_and_hasher(pu.len(), Default::default());
        let mut extern_funcs = FxHashMap::with_capacity_and_hasher(pu.len(), Default::default());

        for (key, code) in pu.iter() {
            let ids: FxHashSet<String> = tokenize_c_identifiers(code)
                .map(|s| s.to_string())
                .collect();
            let extern_fs = extract_extern_declared_functions(code);
            identifiers.insert(key.clone(), ids);
            extern_funcs.insert(key.clone(), extern_fs);
        }

        CodeIdentifiers { identifiers, extern_funcs }
    }

    /// Get identifiers for a given PU key
    #[inline]
    pub fn get(&self, key: &str) -> Option<&FxHashSet<String>> {
        self.identifiers.get(key)
    }

    /// Check if a PU's code contains a specific identifier
    #[inline]
    pub fn contains(&self, key: &str, identifier: &str) -> bool {
        self.identifiers.get(key).map_or(false, |ids| ids.contains(identifier))
    }

    /// Get union of identifiers from multiple PU keys
    pub fn get_union<'a, I>(&self, keys: I) -> FxHashSet<&str>
    where
        I: Iterator<Item = &'a String>,
    {
        let mut result = FxHashSet::default();
        for key in keys {
            if let Some(ids) = self.identifiers.get(key) {
                for id in ids {
                    result.insert(id.as_str());
                }
            }
        }
        result
    }

    /// Get union of extern-declared function names from multiple PU keys (Optimization #3)
    pub fn get_extern_funcs_union<'a, I>(&self, keys: I) -> FxHashSet<&str>
    where
        I: Iterator<Item = &'a String>,
    {
        let mut result = FxHashSet::default();
        for key in keys {
            if let Some(funcs) = self.extern_funcs.get(key) {
                for f in funcs {
                    result.insert(f.as_str());
                }
            }
        }
        result
    }
}

impl PrecomputedStructures {
    /// Build all pre-computed structures from the given inputs
    /// This is called once before parallel processing to avoid rebuilding for each PU
    pub fn build(
        pu_order: &[String],
        dep: &FxHashMap<String, Vec<String>>,
        tags: &FxHashMap<String, Vec<String>>,
        enumerator_to_enum: &FxHashMap<String, String>,
        system_typedefs: &[(String, String)],
        pu: &FxHashMap<String, String>,
    ) -> Self {
        // Pre-compute shared data structures ONCE before parallel processing
        // Bug60 fix: Pass enumerator_to_enum for resolving enum constant dependencies
        let shared_maps = SharedMaps::from_tags(tags, enumerator_to_enum);

        // Pre-parse tags for O(1) pu_key lookup (avoids repeated splitn parsing)
        let parsed_tags = build_parsed_tags(tags);

        // Pre-compute transitive dependencies for all keys
        // (use build_with_filter for optimized PU_FILTER mode)
        let transitive_deps = TransitiveDeps::compute(pu_order, dep, &parsed_tags, enumerator_to_enum);

        // Build PositionIndex ONCE before parallel loop
        let position_index = PositionIndex::from_pu_order(pu_order);

        // Pre-compute project types ONCE before parallel loop
        let project_types = ProjectTypes::build(pu_order, pu, system_typedefs);

        // Pre-tokenize all code blocks (Optimization #1)
        let code_identifiers = CodeIdentifiers::build(pu);

        // Build global interner (Optimization #2)
        let interner = GlobalInterner::build(pu_order, tags);

        // Build interned data structures for fast InternId-based operations
        let interned_trans_deps = InternedTransitiveDeps::from_transitive_deps(&transitive_deps, &interner);
        let interned_pos_index = InternedPositionIndex::from_position_index(&position_index, &interner);

        PrecomputedStructures {
            shared_maps,
            parsed_tags,
            transitive_deps,
            position_index,
            project_types,
            code_identifiers,
            interner,
            interned_trans_deps,
            interned_pos_index,
        }
    }

    /// Build with optimized transitive deps computation for PU_FILTER case
    /// This version takes uids to enable filtering computation
    pub fn build_with_filter(
        pu_order: &[String],
        dep: &FxHashMap<String, Vec<String>>,
        tags: &FxHashMap<String, Vec<String>>,
        enumerator_to_enum: &FxHashMap<String, String>,
        system_typedefs: &[(String, String)],
        pu: &FxHashMap<String, String>,
        config: &SplitConfig,
        uids: &FxHashMap<String, usize>,
    ) -> Self {
        // Bug60 fix: Pass enumerator_to_enum for resolving enum constant dependencies
        let shared_maps = SharedMaps::from_tags(tags, enumerator_to_enum);
        let parsed_tags = build_parsed_tags(tags);

        // Optimized transitive deps computation for PU_FILTER
        let transitive_deps = if let Some(ref filter) = config.pu_filter {
            // Find the maximum position of any target PU in pu_order
            let mut max_pos: Option<usize> = None;
            for (i, u) in pu_order.iter().enumerate() {
                if let Some(&uid) = uids.get(u) {
                    if filter.contains(&uid) {
                        max_pos = Some(match max_pos {
                            Some(prev) => prev.max(i),
                            None => i,
                        });
                    }
                }
            }

            if let Some(max_position) = max_pos {
                // Compute transitive deps for all keys up to and including max target position
                let keys_to_compute = &pu_order[0..max_position + 1];
                eprintln!("[LAZY_DEPS] Computing transitive deps for {} PUs (up to position {})",
                    keys_to_compute.len(), max_position);
                TransitiveDeps::compute(keys_to_compute, dep, &parsed_tags, enumerator_to_enum)
            } else {
                eprintln!("[LAZY_DEPS] No target PUs found in pu_order");
                TransitiveDeps::empty()
            }
        } else {
            TransitiveDeps::compute(pu_order, dep, &parsed_tags, enumerator_to_enum)
        };

        let position_index = PositionIndex::from_pu_order(pu_order);
        let project_types = ProjectTypes::build(pu_order, pu, system_typedefs);
        let code_identifiers = CodeIdentifiers::build(pu);

        // Build global interner (Optimization #2)
        let interner = GlobalInterner::build(pu_order, tags);

        // Build interned data structures for fast InternId-based operations
        let interned_trans_deps = InternedTransitiveDeps::from_transitive_deps(&transitive_deps, &interner);
        let interned_pos_index = InternedPositionIndex::from_position_index(&position_index, &interner);

        PrecomputedStructures {
            shared_maps,
            parsed_tags,
            transitive_deps,
            position_index,
            project_types,
            code_identifiers,
            interner,
            interned_trans_deps,
            interned_pos_index,
        }
    }
}

// ============================================================================
// Parsed Tag Value - Optimization #2
// Pre-parses tag values to avoid repeated splitn(3, ':') in hot path
// ============================================================================

/// Pre-parsed tag value containing the type, name, and pu_key
/// This eliminates splitn overhead during transitive dependency computation
#[derive(Clone)]
pub struct ParsedTagValue {
    /// The PU type (function, variable, typedef, etc.)
    pub pu_type: PuType,
    /// The pre-constructed pu_key (type:name:file format)
    pub pu_key: String,
}

/// Parsed tags map: name -> Vec<ParsedTagValue>
/// This replaces the string-based tags map for hot path lookups
pub type ParsedTagsMap = FxHashMap<String, Vec<ParsedTagValue>>;

/// Build parsed tags map from raw tags and pu_order
/// Called once during initialization to pre-parse all tag values
pub fn build_parsed_tags(
    tags: &FxHashMap<String, Vec<String>>,
) -> ParsedTagsMap {
    let mut parsed: ParsedTagsMap = FxHashMap::with_capacity_and_hasher(tags.len(), Default::default());

    // OPTIMIZATION: Pre-allocate buffer for building keys, reuse across iterations
    let mut key_buf = String::with_capacity(128);

    for (name, values) in tags.iter() {
        let mut parsed_values = Vec::with_capacity(values.len());
        for u_val in values.iter() {
            // Parse "type:file" format from tags value
            let parts: Vec<&str> = u_val.splitn(2, ':').collect();
            if parts.len() == 2 {
                let type_str = parts[0];
                let file_str = parts[1];
                let pu_type = PuType::from_str(type_str);
                // Construct pu_key: "type:name:file" using buffer
                key_buf.clear();
                key_buf.push_str(type_str);
                key_buf.push(':');
                key_buf.push_str(name);
                key_buf.push(':');
                key_buf.push_str(file_str);
                parsed_values.push(ParsedTagValue { pu_type, pu_key: key_buf.clone() });
            }
        }
        if !parsed_values.is_empty() {
            parsed.insert(name.clone(), parsed_values);
        }
    }

    parsed
}

/// Pre-compiled regex for word tokenization - compiled once, shared across all threads
/// NOTE: This is now a fallback - prefer using tokenize_c_identifiers() for performance
#[allow(dead_code)]
static WORD_PATTERN: Lazy<regex::Regex> = Lazy::new(|| {
    regex::Regex::new(r"\b[a-zA-Z_][a-zA-Z0-9_]*\b").unwrap()
});

/// Pre-compiled regex for extern function declarations
/// Pattern: extern ... function_name ( ...
/// Captures the function name in group 1
/// Bug62 fix: Handle pointer-to-pointer returns like `extern int **func(` where ** is
/// directly adjacent to the function name with no intervening whitespace.
/// Uses alternation: (type words with trailing space)+ then optional (*+ with optional space) then funcname
/// Excludes function pointer variables like `extern int (*name)(` by requiring type word(s) before *.
static EXTERN_FUNC_RE: Lazy<regex::Regex> = Lazy::new(|| {
    regex::Regex::new(r"^extern\s+(?:[a-zA-Z_][a-zA-Z0-9_]*\s+)+(?:\*+\s*)?([a-zA-Z_][a-zA-Z0-9_]*)\s*\(").unwrap()
});

/// Fast C identifier tokenizer - avoids regex overhead entirely
/// This is equivalent to the regex pattern \b[a-zA-Z_][a-zA-Z0-9_]*\b
/// but runs ~10x faster by using direct character comparisons.
#[inline(always)]
fn is_ident_start(c: u8) -> bool {
    matches!(c, b'a'..=b'z' | b'A'..=b'Z' | b'_')
}

#[inline(always)]
fn is_ident_char(c: u8) -> bool {
    matches!(c, b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_')
}

/// Tokenize a string into C identifiers without using regex.
/// Returns an iterator that yields &str slices for each identifier found.
/// This is significantly faster than regex for simple identifier patterns.
#[inline]
fn tokenize_c_identifiers(s: &str) -> impl Iterator<Item = &str> {
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    std::iter::from_fn(move || {
        // Skip non-identifier characters, including string and char literals
        while i < len && !is_ident_start(bytes[i]) {
            // Skip string literals
            if bytes[i] == b'"' {
                i += 1;
                while i < len && bytes[i] != b'"' {
                    if bytes[i] == b'\\' && i + 1 < len {
                        i += 2; // skip escaped char
                    } else {
                        i += 1;
                    }
                }
                if i < len {
                    i += 1; // skip closing quote
                }
                continue;
            }
            // Skip character literals
            if bytes[i] == b'\'' {
                i += 1;
                while i < len && bytes[i] != b'\'' {
                    if bytes[i] == b'\\' && i + 1 < len {
                        i += 2; // skip escaped char
                    } else {
                        i += 1;
                    }
                }
                if i < len {
                    i += 1; // skip closing quote
                }
                continue;
            }
            i += 1;
        }

        if i >= len {
            return None;
        }

        let start = i;
        i += 1;

        // Consume identifier characters
        while i < len && is_ident_char(bytes[i]) {
            i += 1;
        }

        // Safe: C identifiers are ASCII, which is valid UTF-8
        Some(std::str::from_utf8(&bytes[start..i]).unwrap())
    })
}

/// Extract unique identifiers from code into a HashSet.
/// This tokenizes once and stores results for O(1) membership tests.
#[inline]
#[allow(dead_code)]
fn extract_identifiers(code: &str) -> FxHashSet<&str> {
    tokenize_c_identifiers(code).collect()
}

/// Check if any identifier from a set appears in the code.
/// More efficient than iterating through code for each identifier when
/// the set is large and code is scanned multiple times.
#[inline]
#[allow(dead_code)]
fn code_contains_identifier(code_identifiers: &FxHashSet<&str>, name: &str) -> bool {
    code_identifiers.contains(name)
}

/// Extract all function names that have extern declarations in the code.
/// Pattern: `extern ... func_name(` - finds function names in extern declarations
/// Uses a manual state machine parser instead of regex for ~2x speedup.
/// Returns a HashSet for O(1) lookup.
/// This is called ONCE per PU, then we do O(1) lookups for each candidate function.
#[inline]
fn extract_extern_declared_functions(code: &str) -> rustc_hash::FxHashSet<String> {
    let mut result = rustc_hash::FxHashSet::default();
    let bytes = code.as_bytes();
    let len = bytes.len();

    // Use memchr to find potential "extern" keywords quickly
    let mut i = 0;
    while i + 6 <= len {
        // Look for 'e' that might start "extern"
        if let Some(pos) = memchr::memchr(b'e', &bytes[i..]) {
            i += pos;
            // Check if this is "extern" followed by whitespace
            if i + 6 <= len && &bytes[i..i+6] == b"extern" {
                let after_extern = i + 6;
                // Must be followed by whitespace (not part of longer identifier)
                if after_extern < len && (bytes[after_extern] == b' ' || bytes[after_extern] == b'\t' || bytes[after_extern] == b'\n' || bytes[after_extern] == b'\r') {
                    // Also check that "extern" is not preceded by an identifier char
                    let valid_start = i == 0 || !is_ident_char(bytes[i - 1]);
                    if valid_start {
                        // Found "extern " - now find the function name before '('
                        // Skip whitespace and find the identifier right before '('
                        let mut j = after_extern;

                        // Track the last identifier we see
                        let mut last_ident_start: Option<usize> = None;
                        let mut last_ident_end: Option<usize> = None;

                        while j < len {
                            let b = bytes[j];
                            if b == b'(' {
                                // Found '(' - the last identifier is the function name
                                if let (Some(start), Some(end)) = (last_ident_start, last_ident_end) {
                                        // Safe: C identifiers are ASCII
                                    let func_name = std::str::from_utf8(&bytes[start..end]).unwrap();
                                    result.insert(func_name.to_string());
                                }
                                i = j + 1;
                                break;
                            } else if b == b';' {
                                // Hit semicolon without '(' - not a function declaration
                                i = j + 1;
                                break;
                            } else if is_ident_start(b) {
                                // Start of an identifier
                                let ident_start = j;
                                j += 1;
                                while j < len && is_ident_char(bytes[j]) {
                                    j += 1;
                                }
                                last_ident_start = Some(ident_start);
                                last_ident_end = Some(j);
                                continue;
                            }
                            j += 1;
                        }
                        if j >= len {
                            break;
                        }
                        continue;
                    }
                }
            }
            i += 1;
        } else {
            break;
        }
    }

    result
}

/// Check if code already has an extern declaration for the given function name.
/// Uses regex pattern `extern\s+[^;]*\bfunc_name\s*\(`
/// which checks for "extern" followed by any chars except ';' then the function name and '('
/// NOTE: This is kept for backwards compatibility but extract_extern_declared_functions is preferred.
#[allow(dead_code)]
#[inline]
fn has_extern_declaration(code: &str, func_name: &str) -> bool {
    let extern_pattern = format!(r"extern\s+[^;]*\b{}\s*\(", regex::escape(func_name));
    if let Ok(re) = regex::Regex::new(&extern_pattern) {
        re.is_match(code)
    } else {
        false
    }
}

/// Static set of C type keywords used for return type detection
/// This is used to efficiently check if a string contains type indicators
static RETURN_TYPE_KEYWORDS: Lazy<FxHashSet<&'static str>> = Lazy::new(|| {
    [
        "void", "int", "char", "static", "extern", "unsigned", "long",
        "uint32_t", "int32_t", "char_u", "short", "float", "double",
        "const", "volatile", "signed", "inline", "__inline", "__inline__",
    ].iter().copied().collect()
});

/// Check if a string contains return type indicators efficiently.
/// Uses tokenization + HashSet lookup instead of multiple contains() calls.
/// This is O(n) where n is string length, vs O(n*k) for k contains() calls.
#[inline]
fn contains_return_type_keyword(s: &str) -> bool {
    // Quick check for pointer return types via suffix
    let trimmed = s.trim();
    if trimmed.ends_with('*') || trimmed.ends_with("* ") || trimmed.ends_with("_t ") || trimmed.ends_with("_t") {
        return true;
    }
    // Check for void * pattern
    if trimmed.contains("void *") || trimmed.contains("void*") {
        return true;
    }
    // Tokenize and check against keyword set
    tokenize_c_identifiers(s).any(|token| RETURN_TYPE_KEYWORDS.contains(token))
}

/// Check if a string is a complete type line (for K&R style detection).
/// Returns true if the entire trimmed line is a type keyword or type pattern.
#[inline]
fn is_type_only_line(trimmed: &str) -> bool {
    if trimmed.is_empty() {
        return false;
    }
    // Check for exact matches of common type patterns
    match trimmed {
        "void" | "static void" | "static" | "extern void" | "extern" |
        "int" | "char" | "unsigned" | "long" | "short" => return true,
        _ => {}
    }
    // Check for prefix patterns
    if trimmed.starts_with("static ") || trimmed.starts_with("extern ") {
        return true;
    }
    // Check for suffix patterns (pointer type on its own line)
    if trimmed.ends_with('*') || trimmed.ends_with("_t") {
        return true;
    }
    // Check for void * pattern
    if trimmed.contains("void *") {
        return true;
    }
    // Check for type keywords
    if trimmed.contains("char_u") || trimmed.contains("uint32_t") || trimmed.contains("int32_t") {
        return true;
    }
    false
}

/// String-based version of has_extern_declaration (kept as dead code for reference)
/// This was slower than the regex version in benchmarks.
#[allow(dead_code)]
fn has_extern_declaration_string_based(code: &str, func_name: &str) -> bool {
    // Quick check: if either keyword is missing, can't be a match
    if !code.contains("extern") {
        return false;
    }

    // Build the search pattern: func_name followed by optional whitespace and '('
    let func_with_paren = format!("{}(", func_name);
    let func_with_space_paren = format!("{} (", func_name);

    // Find all "extern" occurrences and check if func_name( appears before the next ';'
    let bytes = code.as_bytes();
    let extern_bytes = b"extern";

    let mut i = 0;
    while i + 6 <= bytes.len() {
        // Find "extern" keyword
        if &bytes[i..i+6] == extern_bytes {
            // Make sure it's a word boundary (not part of another identifier)
            let is_word_start = i == 0 || !is_ident_char(bytes[i - 1]);
            let is_word_end = i + 6 >= bytes.len() || !is_ident_char(bytes[i + 6]);

            if is_word_start && is_word_end {
                // Find the next semicolon - this is the end of the potential extern declaration
                let decl_end = bytes[i..].iter().position(|&b| b == b';')
                    .map(|pos| i + pos)
                    .unwrap_or(bytes.len());

                // Check if func_name( or func_name ( appears in this range
                let decl_slice = &code[i..decl_end];
                if decl_slice.contains(&func_with_paren) || decl_slice.contains(&func_with_space_paren) {
                    return true;
                }

                // Move past this extern declaration
                i = decl_end;
            }
        }
        i += 1;
    }

    false
}

/// Fast function call scanner - finds identifiers followed by '('
/// This replaces the regex pattern `([a-zA-Z_][a-zA-Z0-9_]*)\s*\(`
/// Returns an iterator yielding function names (identifiers before '(')
#[inline]
fn tokenize_function_calls(s: &str) -> impl Iterator<Item = &str> {
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    std::iter::from_fn(move || {
        loop {
            // Skip non-identifier characters
            while i < len && !is_ident_start(bytes[i]) {
                i += 1;
            }

            if i >= len {
                return None;
            }

            let start = i;
            i += 1;

            // Consume identifier characters
            while i < len && is_ident_char(bytes[i]) {
                i += 1;
            }

            let ident_end = i;

            // Skip whitespace after identifier
            while i < len && (bytes[i] == b' ' || bytes[i] == b'\t' || bytes[i] == b'\n' || bytes[i] == b'\r') {
                i += 1;
            }

            // Check if followed by '('
            if i < len && bytes[i] == b'(' {
                // Safe: C identifiers are ASCII
                return Some(std::str::from_utf8(&bytes[start..ident_end]).unwrap());
            }
            // If not followed by '(', continue searching from current position
        }
    })
}

/// Pre-computed shared data structures for dependency computation
/// These are built once before the parallel loop and shared as read-only across all threads
pub struct SharedMaps {
    /// typedef name -> Vec<pu_key> (e.g., "typedef:sqlite3StatValueType:sqlite3.i")
    pub typedef_map: FxHashMap<String, Vec<String>>,
    /// prototype name -> Vec<unit_key>
    pub prototype_map: FxHashMap<String, Vec<String>>,
    /// function name -> Vec<unit_key>
    pub function_map: FxHashMap<String, Vec<String>>,
    /// struct name -> Vec<pu_key> (e.g., "struct:__jmp_buf_tag:bug36.i")
    pub struct_map: FxHashMap<String, Vec<String>>,
    /// union name -> Vec<pu_key>
    pub union_map: FxHashMap<String, Vec<String>>,
    /// enumerator name -> parent enum pu_key (e.g., "KS_XON" -> "enum:SpecialKey:vim_amalg.i")
    /// Bug60 fix: Maps enumerator constants to their parent enum for dependency resolution
    pub enumerator_map: FxHashMap<String, String>,
    /// All typedef names for quick lookup
    pub all_typedef_names: FxHashSet<String>,
    /// All function/prototype names for quick lookup
    pub all_func_names: FxHashSet<String>,
    /// All struct names for quick lookup
    pub all_struct_names: FxHashSet<String>,
    /// All union names for quick lookup
    pub all_union_names: FxHashSet<String>,
    /// All enumerator names for quick lookup (Bug60 fix)
    pub all_enumerator_names: FxHashSet<String>,
    /// All tag names for quick lookup (optimization: avoids rebuilding per-PU)
    pub all_tag_names: FxHashSet<String>,
    /// Names that have at least one non-function unit (typedef, struct, union, enum, etc.)
    /// Used for Bug50 fix optimization - avoids rebuilding per-PU
    pub non_function_names: FxHashSet<String>,
    /// static variable name -> Vec<pu_key> (e.g., "rex" -> "variable:rex:regexp.i")
    /// Used to add file-scope static variable declarations as dependencies
    pub variable_map: FxHashMap<String, Vec<String>>,
    /// All variable names for quick lookup
    pub all_variable_names: FxHashSet<String>,
}

/// Pre-computed project types - type names defined in the project with non-empty bodies
/// This is used to filter system typedefs that conflict with project definitions
/// OPTIMIZATION: Built once before parallel processing, shared across all PUs
pub struct ProjectTypes {
    /// Set of type names (typedef, enum, struct, union, enumerator) with non-empty bodies
    pub types: FxHashSet<String>,
}

impl ProjectTypes {
    /// Build project types from pu_order, pu bodies, and system_typedefs
    /// Called ONCE before parallel processing to avoid O(num_pus * pu_order) overhead
    pub fn build(
        pu_order: &[String],
        pu: &FxHashMap<String, String>,
        system_typedefs: &[(String, String)],
    ) -> Self {
        // Build set of system typedef names for filtering.
        // Also include well-known stdbool.h names that are NOT __-prefixed but
        // must still be treated as system-level: only the FIRST .pu.c that needs
        // them will emit the definition; all others should treat them as already
        // declared.  Without this, every .pu.c that uses `bool` re-emits
        // `enum { false=0, true=1 }` causing "redeclaration of enumerator 'false'".
        let stdbool_system_names: &[&str] = &["bool", "_Bool", "false", "true"];
        let system_typedef_names: FxHashSet<&str> = system_typedefs.iter()
            .map(|(name, _)| name.as_str())
            .chain(stdbool_system_names.iter().copied())
            .collect();

        let types: FxHashSet<String> = pu_order.iter()
            .filter_map(|u| {
                let parts: Vec<&str> = u.split(':').collect();
                if parts.len() >= 2 {
                    let type_kind = parts[0];
                    let type_name = parts[1];
                    // Skip if this type is already in system_typedefs - system typedefs take precedence
                    if system_typedef_names.contains(type_name) {
                        return None;
                    }
                    // Include typedefs, enums, structs, unions, and enumerators as project-defined types
                    // BUT only if they have a non-empty body in pu
                    if type_kind == "typedef" || type_kind == "enum" || type_kind == "struct"
                        || type_kind == "union" || type_kind == "enumerator" {
                        // Check if this type has a non-empty body
                        if let Some(body) = pu.get(u.as_str()) {
                            if !body.trim().is_empty() {
                                Some(type_name.to_string())
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .collect();

        ProjectTypes { types }
    }

    /// Check if a type name is defined in the project
    #[inline]
    pub fn contains(&self, name: &str) -> bool {
        self.types.contains(name)
    }
}

impl SharedMaps {
    /// Build shared maps from tags and enumerator_to_enum - called ONCE before parallel processing
    /// Bug60 fix: Now also includes enumerator_map for resolving enum constant dependencies
    pub fn from_tags(tags: &FxHashMap<String, Vec<String>>, enumerator_to_enum: &FxHashMap<String, String>) -> Self {
        // Pre-size HashMaps based on tags size to avoid rehashing
        // Typical ratios observed in SQLite: ~10% typedefs, ~20% prototypes, ~30% functions
        let estimated_size = tags.len();
        let mut typedef_map: FxHashMap<String, Vec<String>> =
            FxHashMap::with_capacity_and_hasher(estimated_size / 10, Default::default());
        let mut prototype_map: FxHashMap<String, Vec<String>> =
            FxHashMap::with_capacity_and_hasher(estimated_size / 5, Default::default());
        let mut function_map: FxHashMap<String, Vec<String>> =
            FxHashMap::with_capacity_and_hasher(estimated_size / 3, Default::default());
        let mut struct_map: FxHashMap<String, Vec<String>> =
            FxHashMap::with_capacity_and_hasher(estimated_size / 20, Default::default());
        let mut union_map: FxHashMap<String, Vec<String>> =
            FxHashMap::with_capacity_and_hasher(estimated_size / 50, Default::default());
        let mut variable_map: FxHashMap<String, Vec<String>> =
            FxHashMap::with_capacity_and_hasher(estimated_size / 20, Default::default());

        // OPTIMIZATION: Pre-allocate buffer for building keys, reuse across iterations
        let mut key_buf = String::with_capacity(128);

        for (name, units) in tags.iter() {
            for unit in units.iter() {
                // Use PuType::from_key for O(1) first-byte dispatch instead of chained starts_with()
                // Use efficient parse_key_type_rest instead of splitn().collect()
                match PuType::from_key(unit) {
                    PuType::Typedef => {
                        // Unit format is "typedef:filename" (e.g., "typedef:sqlite3.i")
                        // Pu key format is "typedef:name:filename" (e.g., "typedef:sqlite3StatValueType:sqlite3.i")
                        if let Some((_, filename)) = parse_key_type_rest(unit) {
                            key_buf.clear();
                            key_buf.push_str("typedef:");
                            key_buf.push_str(name);
                            key_buf.push(':');
                            key_buf.push_str(filename);
                            typedef_map.entry(name.clone()).or_default().push(key_buf.clone());
                        }
                    }
                    PuType::Prototype => {
                        // Build full pu_key format for consistency with other maps
                        // Unit format is "prototype:filename", build "prototype:name:filename"
                        if let Some((_, filename)) = parse_key_type_rest(unit) {
                            key_buf.clear();
                            key_buf.push_str("prototype:");
                            key_buf.push_str(name);
                            key_buf.push(':');
                            key_buf.push_str(filename);
                            prototype_map.entry(name.clone()).or_default().push(key_buf.clone());
                        }
                    }
                    PuType::Function => {
                        // Bug69 fix: Store raw unit format "function:filename" (without name)
                        // This creates deliberate mismatch with pu_order "function:name:filename"
                        // so that function bodies added by scan_for_prototype_references are NOT
                        // output when iterating pu_order. Only K&R forward declarations are generated.
                        function_map.entry(name.clone()).or_default().push(unit.clone());
                    }
                    PuType::Struct => {
                        // Unit format is "struct:filename" (e.g., "struct:sqlite3.i")
                        // Pu key format is "struct:name:filename" (e.g., "struct:__jmp_buf_tag:bug36.i")
                        if let Some((_, filename)) = parse_key_type_rest(unit) {
                            key_buf.clear();
                            key_buf.push_str("struct:");
                            key_buf.push_str(name);
                            key_buf.push(':');
                            key_buf.push_str(filename);
                            struct_map.entry(name.clone()).or_default().push(key_buf.clone());
                        }
                    }
                    PuType::Union => {
                        // Unit format is "union:filename"
                        if let Some((_, filename)) = parse_key_type_rest(unit) {
                            key_buf.clear();
                            key_buf.push_str("union:");
                            key_buf.push_str(name);
                            key_buf.push(':');
                            key_buf.push_str(filename);
                            union_map.entry(name.clone()).or_default().push(key_buf.clone());
                        }
                    }
                    PuType::Variable => {
                        // Unit format is "variable:filename"
                        if let Some((_, filename)) = parse_key_type_rest(unit) {
                            key_buf.clear();
                            key_buf.push_str("variable:");
                            key_buf.push_str(name);
                            key_buf.push(':');
                            key_buf.push_str(filename);
                            variable_map.entry(name.clone()).or_default().push(key_buf.clone());
                        }
                    }
                    _ => {} // Skip other types
                }
            }
        }

        let all_typedef_names: FxHashSet<String> = typedef_map.keys().cloned().collect();
        let all_func_names: FxHashSet<String> = prototype_map.keys()
            .chain(function_map.keys())
            .cloned()
            .collect();
        let all_struct_names: FxHashSet<String> = struct_map.keys().cloned().collect();
        let all_union_names: FxHashSet<String> = union_map.keys().cloned().collect();
        let all_variable_names: FxHashSet<String> = variable_map.keys().cloned().collect();

        // Bug60 fix: Build enumerator_map from enumerator_to_enum
        // This maps enumerator names (e.g., "KS_XON") to their parent enum pu_key
        let enumerator_map: FxHashMap<String, String> = enumerator_to_enum.clone();
        let all_enumerator_names: FxHashSet<String> = enumerator_map.keys().cloned().collect();

        // OPTIMIZATION: Pre-compute all_tag_names (avoids O(tags) per PU)
        let all_tag_names: FxHashSet<String> = tags.keys().cloned().collect();

        // OPTIMIZATION: Pre-compute non_function_names (avoids O(tags * units) per PU)
        // These are names that have at least one non-function unit (typedef, struct, union, etc.)
        let non_function_names: FxHashSet<String> = tags.iter()
            .filter(|(_, units)| {
                units.iter().any(|u| PuType::key_is_type_def(u))
            })
            .map(|(name, _)| name.clone())
            .collect();

        SharedMaps {
            typedef_map,
            prototype_map,
            function_map,
            struct_map,
            union_map,
            enumerator_map,
            all_typedef_names,
            all_func_names,
            all_struct_names,
            all_union_names,
            all_enumerator_names,
            all_tag_names,
            non_function_names,
            variable_map,
            all_variable_names,
        }
    }
}

// ============================================================================
// Indexed Dependency Graph - Optimization #4
// Uses u32 indices instead of String for dependency computation
// Eliminates millions of string clones and hash operations
// ============================================================================

/// Indexed pu_key interner for fast dependency computation
/// Maps pu_keys to u32 indices and back
pub struct PuKeyIndex {
    /// pu_key -> index
    key_to_idx: FxHashMap<String, u32>,
    /// index -> pu_key (for reverse lookup)
    idx_to_key: Vec<String>,
}

impl PuKeyIndex {
    /// Create an empty PuKeyIndex
    pub fn empty() -> Self {
        PuKeyIndex {
            key_to_idx: FxHashMap::default(),
            idx_to_key: Vec::new(),
        }
    }

    /// Build index from pu_order and parsed_tags
    pub fn build(pu_order: &[String], parsed_tags: &ParsedTagsMap, enumerator_to_enum: &FxHashMap<String, String>) -> Self {
        // Collect all unique pu_keys from pu_order, parsed_tags, and enumerator_to_enum
        let mut all_keys: FxHashSet<String> = FxHashSet::default();

        // Add all pu_order keys
        for key in pu_order {
            all_keys.insert(key.clone());
        }

        // Add all pu_keys from parsed_tags
        for values in parsed_tags.values() {
            for parsed in values {
                all_keys.insert(parsed.pu_key.clone());
            }
        }

        // Add all enum parent keys from enumerator_to_enum
        for parent_key in enumerator_to_enum.values() {
            all_keys.insert(parent_key.clone());
        }

        // Build bidirectional mapping
        let mut key_to_idx = FxHashMap::with_capacity_and_hasher(all_keys.len(), Default::default());
        let mut idx_to_key = Vec::with_capacity(all_keys.len());

        for key in all_keys {
            let idx = idx_to_key.len() as u32;
            key_to_idx.insert(key.clone(), idx);
            idx_to_key.push(key);
        }

        PuKeyIndex { key_to_idx, idx_to_key }
    }

    #[inline]
    pub fn get_idx(&self, key: &str) -> Option<u32> {
        self.key_to_idx.get(key).copied()
    }

    #[inline]
    pub fn get_key(&self, idx: u32) -> &str {
        &self.idx_to_key[idx as usize]
    }

    pub fn len(&self) -> usize {
        self.idx_to_key.len()
    }
}

/// Indexed parsed tags: dep_name -> Vec<(pu_type, pu_key_idx)>
pub struct IndexedParsedTags {
    /// dep_name -> Vec<(PuType, pu_key_idx)>
    map: FxHashMap<String, Vec<(PuType, u32)>>,
}

impl IndexedParsedTags {
    pub fn build(parsed_tags: &ParsedTagsMap, key_index: &PuKeyIndex) -> Self {
        let mut map = FxHashMap::with_capacity_and_hasher(parsed_tags.len(), Default::default());

        for (name, values) in parsed_tags {
            let indexed: Vec<(PuType, u32)> = values.iter()
                .filter_map(|parsed| {
                    key_index.get_idx(&parsed.pu_key).map(|idx| (parsed.pu_type, idx))
                })
                .collect();
            if !indexed.is_empty() {
                map.insert(name.clone(), indexed);
            }
        }

        IndexedParsedTags { map }
    }

    #[inline]
    pub fn get(&self, name: &str) -> Option<&Vec<(PuType, u32)>> {
        self.map.get(name)
    }
}

/// Indexed enumerator to enum map: enumerator_name -> parent_enum_pu_key_idx
pub struct IndexedEnumMap {
    map: FxHashMap<String, u32>,
}

impl IndexedEnumMap {
    pub fn build(enumerator_to_enum: &FxHashMap<String, String>, key_index: &PuKeyIndex) -> Self {
        let mut map = FxHashMap::with_capacity_and_hasher(enumerator_to_enum.len(), Default::default());

        for (name, parent_key) in enumerator_to_enum {
            if let Some(idx) = key_index.get_idx(parent_key) {
                map.insert(name.clone(), idx);
            }
        }

        IndexedEnumMap { map }
    }

    #[inline]
    pub fn get(&self, name: &str) -> Option<u32> {
        self.map.get(name).copied()
    }
}

/// Iterator over set bits in a u64 word
/// Used for efficient iteration over bit-vector dependencies
struct BitIter {
    word: u64,
    base: usize,
    max_idx: usize,
}

impl BitIter {
    #[inline]
    fn new(word: u64, base: usize, max_idx: usize) -> Self {
        BitIter { word, base, max_idx }
    }
}

impl Iterator for BitIter {
    type Item = u32;

    #[inline]
    fn next(&mut self) -> Option<u32> {
        if self.word == 0 {
            return None;
        }
        let bit = self.word.trailing_zeros() as usize;
        let idx = self.base + bit;
        self.word &= self.word - 1; // Clear lowest set bit
        if idx < self.max_idx {
            Some(idx as u32)
        } else {
            None
        }
    }
}

/// Pre-computed transitive dependencies for each PU
/// Maps: pu_key -> set of all transitive dependencies (as pu_keys)
/// NOTE: The lookup uses filtering by valid_keys (built from pu_order slice) to respect
/// the constraint that only dependencies appearing before the current PU can be included.
/// OPTIMIZATION: Internally uses u32 indices during computation to avoid string cloning
/// OPTIMIZATION: Uses topological sort to avoid lock contention - process in order where
/// all dependencies are already computed
/// OPTIMIZATION: Keeps bit-vector representation for fast index-based queries
pub struct TransitiveDeps {
    /// For each pu_key, the set of all transitive dependency pu_keys (for legacy string-based access)
    pub deps: FxHashMap<String, FxHashSet<String>>,
    /// Bit-vector representation: bitvecs[idx] has bit i set if node i is a dependency
    /// This enables O(1) dependency checks without string allocation
    pub bitvecs: Vec<Vec<u64>>,
    /// Key index for string <-> u32 conversion
    pub key_index: PuKeyIndex,
    /// Number of 64-bit words per node
    pub words_per_node: usize,
}

impl TransitiveDeps {
    /// Build the direct dependency graph as adjacency list using u32 indices
    /// Returns: for each node, the set of nodes it directly depends on
    fn build_direct_deps_graph(
        pu_order: &[u32],
        dep: &FxHashMap<String, Vec<String>>,
        indexed_tags: &IndexedParsedTags,
        indexed_enum: &IndexedEnumMap,
        key_index: &PuKeyIndex,
    ) -> Vec<Vec<u32>> {
        let num_keys = key_index.len();
        let mut graph: Vec<Vec<u32>> = vec![Vec::new(); num_keys];

        // Build set of valid indices for fast membership check
        let valid_set: FxHashSet<u32> = pu_order.iter().copied().collect();

        for &key_idx in pu_order {
            let key = key_index.get_key(key_idx);
            let mut deps_for_node: Vec<u32> = Vec::new();

            if let Some(direct_deps) = dep.get(key) {
                for dep_name in direct_deps.iter() {
                    if let Some(parsed_values) = indexed_tags.get(dep_name) {
                        for &(pu_type, dep_idx) in parsed_values {
                            if dep_idx != key_idx && valid_set.contains(&dep_idx) {
                                // Handle enumerator -> parent enum dependency
                                if pu_type == PuType::Enumerator {
                                    if let Some(parent_idx) = indexed_enum.get(dep_name) {
                                        if !deps_for_node.contains(&parent_idx) {
                                            deps_for_node.push(parent_idx);
                                        }
                                    }
                                }
                                if !deps_for_node.contains(&dep_idx) {
                                    deps_for_node.push(dep_idx);
                                }
                            }
                        }
                    }
                }
            }
            graph[key_idx as usize] = deps_for_node;
        }

        graph
    }

    /// Compute topological levels using Kahn's algorithm
    /// Returns Vec of levels, where each level contains nodes that can be processed in parallel
    /// All dependencies of nodes in level N are in levels < N
    fn topological_levels(graph: &[Vec<u32>], nodes: &[u32]) -> Vec<Vec<u32>> {
        let n = graph.len();
        let mut in_degree = vec![0u32; n];
        let node_set: FxHashSet<u32> = nodes.iter().copied().collect();

        // Build reverse graph and in-degrees (only for nodes in our set)
        let mut reverse_graph: Vec<Vec<u32>> = vec![Vec::new(); n];
        for &node in nodes {
            for &dep in &graph[node as usize] {
                if node_set.contains(&dep) {
                    reverse_graph[dep as usize].push(node);
                    in_degree[node as usize] += 1;
                }
            }
        }

        // Start with nodes that have no dependencies (in_degree = 0) - these form level 0
        let mut current_level: Vec<u32> = nodes.iter()
            .filter(|&&n| in_degree[n as usize] == 0)
            .copied()
            .collect();

        let mut levels: Vec<Vec<u32>> = Vec::new();
        let mut processed: FxHashSet<u32> = FxHashSet::default();

        while !current_level.is_empty() {
            let mut next_level: Vec<u32> = Vec::new();

            for &node in &current_level {
                processed.insert(node);
                // For each node that depends on this one, reduce in_degree
                for &dependent in &reverse_graph[node as usize] {
                    in_degree[dependent as usize] -= 1;
                    if in_degree[dependent as usize] == 0 {
                        next_level.push(dependent);
                    }
                }
            }

            levels.push(std::mem::take(&mut current_level));
            current_level = next_level;
        }

        // Handle cycles: any remaining nodes are in cycles, add them as final level
        let remaining: Vec<u32> = nodes.iter()
            .filter(|&&n| !processed.contains(&n))
            .copied()
            .collect();
        if !remaining.is_empty() {
            levels.push(remaining);
        }

        levels
    }

    /// Pre-compute transitive dependencies for all PUs
    /// OPTIMIZATION: Uses topological levels for parallel processing
    /// Nodes in the same level have no dependencies on each other and can be processed in parallel
    /// OPTIMIZATION: Uses bit-vectors for fast set operations
    pub fn compute(
        pu_order: &[String],
        dep: &FxHashMap<String, Vec<String>>,
        parsed_tags: &ParsedTagsMap,
        enumerator_to_enum: &FxHashMap<String, String>,
    ) -> Self {
        use rayon::prelude::*;

        // Build indexed data structures for fast computation
        let key_index = PuKeyIndex::build(pu_order, parsed_tags, enumerator_to_enum);
        let indexed_tags = IndexedParsedTags::build(parsed_tags, &key_index);
        let indexed_enum = IndexedEnumMap::build(enumerator_to_enum, &key_index);

        // Build indexed pu_order
        let indexed_pu_order: Vec<u32> = pu_order.iter()
            .filter_map(|k| key_index.get_idx(k))
            .collect();

        let num_keys = key_index.len();

        // Build direct dependency graph
        let direct_deps = Self::build_direct_deps_graph(
            &indexed_pu_order, dep, &indexed_tags, &indexed_enum, &key_index
        );

        // Compute topological levels - nodes in same level can be processed in parallel
        let levels = Self::topological_levels(&direct_deps, &indexed_pu_order);

        // Result storage using bit-vectors for compact representation
        // Each bit-vector has num_keys bits, where bit i is set if node i is a dependency
        let words_per_node = (num_keys + 63) / 64;
        let mut results: Vec<Vec<u64>> = vec![vec![0u64; words_per_node]; num_keys];

        // Process level by level sequentially
        for level in &levels {
            for &key_idx in level {
                let mut bv = vec![0u64; words_per_node];

                for &dep_idx in &direct_deps[key_idx as usize] {
                    let word = dep_idx as usize / 64;
                    let bit = dep_idx as usize % 64;
                    bv[word] |= 1u64 << bit;

                    let dep_bv = &results[dep_idx as usize];
                    for (i, &w) in dep_bv.iter().enumerate() {
                        bv[i] |= w;
                    }
                }

                results[key_idx as usize] = bv;
            }
        }

        // Convert bit-vectors back to string-based map
        let mut string_deps: FxHashMap<String, FxHashSet<String>> =
            FxHashMap::with_capacity_and_hasher(pu_order.len(), Default::default());

        for pu_key in pu_order {
            if let Some(idx) = key_index.get_idx(pu_key) {
                let bv = &results[idx as usize];
                let mut deps: FxHashSet<String> = FxHashSet::default();

                // Extract set members from bit-vector
                for (word_idx, &word) in bv.iter().enumerate() {
                    if word != 0 {
                        let mut w = word;
                        let base = word_idx * 64;
                        while w != 0 {
                            let bit = w.trailing_zeros() as usize;
                            let dep_idx = base + bit;
                            if dep_idx < num_keys {
                                deps.insert(key_index.get_key(dep_idx as u32).to_string());
                            }
                            w &= w - 1; // Clear lowest set bit
                        }
                    }
                }

                string_deps.insert(pu_key.clone(), deps);
            }
        }

        TransitiveDeps {
            deps: string_deps,
            bitvecs: results,
            key_index,
            words_per_node,
        }
    }

    /// Create an empty TransitiveDeps (for cases where no computation is needed)
    pub fn empty() -> Self {
        TransitiveDeps {
            deps: FxHashMap::default(),
            bitvecs: Vec::new(),
            key_index: PuKeyIndex::empty(),
            words_per_node: 0,
        }
    }

    /// Get transitive dependencies for a key (legacy string-based access)
    #[inline]
    pub fn get(&self, key: &str) -> Option<&FxHashSet<String>> {
        self.deps.get(key)
    }

    /// Get the index for a key (for index-based operations)
    #[inline]
    pub fn get_key_idx(&self, key: &str) -> Option<u32> {
        self.key_index.get_idx(key)
    }

    /// Get the key for an index
    #[inline]
    pub fn get_key(&self, idx: u32) -> &str {
        self.key_index.get_key(idx)
    }

    /// Check if dep_idx is a transitive dependency of key_idx
    /// This is O(1) with no string allocation
    #[inline]
    pub fn has_dep_idx(&self, key_idx: u32, dep_idx: u32) -> bool {
        if (key_idx as usize) >= self.bitvecs.len() {
            return false;
        }
        let word = dep_idx as usize / 64;
        let bit = dep_idx as usize % 64;
        if word >= self.words_per_node {
            return false;
        }
        (self.bitvecs[key_idx as usize][word] & (1u64 << bit)) != 0
    }

    /// Get all transitive dependencies of key_idx as indices
    /// Yields indices without string allocation
    #[inline]
    pub fn get_deps_idx(&self, key_idx: u32) -> impl Iterator<Item = u32> + '_ {
        let bv = &self.bitvecs[key_idx as usize];
        let num_keys = self.key_index.len();
        bv.iter().enumerate().flat_map(move |(word_idx, &word)| {
            let base = word_idx * 64;
            BitIter::new(word, base, num_keys)
        })
    }

    /// Get all transitive dependencies of key_idx, filtered by max_pos
    /// Avoids string allocation by working with indices
    #[inline]
    pub fn get_deps_filtered<'a>(
        &'a self,
        key_idx: u32,
        position_index: &'a PositionIndex,
        max_pos: usize,
    ) -> impl Iterator<Item = u32> + 'a {
        self.get_deps_idx(key_idx).filter(move |&dep_idx| {
            let dep_key = self.key_index.get_key(dep_idx);
            position_index.is_valid(dep_key, max_pos)
        })
    }

    /// Compute transitive dependencies only for specific PU keys (lazy computation)
    /// This is much faster than computing all PUs when only a few are needed (PU_FILTER mode)
    /// OPTIMIZATION: Uses u32 indices internally
    pub fn compute_filtered(
        target_keys: &[String],
        dep: &FxHashMap<String, Vec<String>>,
        parsed_tags: &ParsedTagsMap,
        enumerator_to_enum: &FxHashMap<String, String>,
    ) -> Self {
        // Build indexed data structures
        let key_index = PuKeyIndex::build(target_keys, parsed_tags, enumerator_to_enum);
        let indexed_tags = IndexedParsedTags::build(parsed_tags, &key_index);
        let indexed_enum = IndexedEnumMap::build(enumerator_to_enum, &key_index);

        // Pre-size for expected number of dependencies
        let mut result: FxHashMap<u32, FxHashSet<u32>> =
            FxHashMap::with_capacity_and_hasher(target_keys.len() * 10, Default::default());

        // Pre-size visited set for typical recursion depth
        let mut visited: FxHashSet<u32> =
            FxHashSet::with_capacity_and_hasher(64, Default::default());

        for pu_key in target_keys.iter() {
            if let Some(key_idx) = key_index.get_idx(pu_key) {
                if !result.contains_key(&key_idx) {
                    visited.clear();
                    Self::compute_transitive_for_key_indexed(
                        key_idx, dep, &indexed_tags, &indexed_enum, &key_index, &mut visited, &mut result
                    );
                }
            }
        }

        // Convert back to string-based map
        let mut string_deps: FxHashMap<String, FxHashSet<String>> =
            FxHashMap::with_capacity_and_hasher(result.len(), Default::default());

        for (key_idx, idx_deps) in &result {
            let key = key_index.get_key(*key_idx).to_string();
            let str_deps: FxHashSet<String> = idx_deps.iter()
                .map(|&i| key_index.get_key(i).to_string())
                .collect();
            string_deps.insert(key, str_deps);
        }

        // Build bit-vectors from the index-based result
        let num_keys = key_index.len();
        let words_per_node = (num_keys + 63) / 64;
        let mut bitvecs: Vec<Vec<u64>> = vec![vec![0u64; words_per_node]; num_keys];

        for (key_idx, idx_deps) in result {
            let bv = &mut bitvecs[key_idx as usize];
            for &dep_idx in &idx_deps {
                let word = dep_idx as usize / 64;
                let bit = dep_idx as usize % 64;
                bv[word] |= 1u64 << bit;
            }
        }

        TransitiveDeps {
            deps: string_deps,
            bitvecs,
            key_index,
            words_per_node,
        }
    }

    /// DFS to compute transitive dependencies for a single key using indices
    fn compute_transitive_for_key_indexed(
        key_idx: u32,
        dep: &FxHashMap<String, Vec<String>>,
        indexed_tags: &IndexedParsedTags,
        indexed_enum: &IndexedEnumMap,
        key_index: &PuKeyIndex,
        visiting: &mut FxHashSet<u32>,
        memo: &mut FxHashMap<u32, FxHashSet<u32>>,
    ) {
        // Already computed
        if memo.contains_key(&key_idx) {
            return;
        }

        // Cycle detection
        if visiting.contains(&key_idx) {
            return;
        }

        visiting.insert(key_idx);

        let key = key_index.get_key(key_idx);

        // Pre-size the set for typical number of dependencies
        let mut transitive_deps: FxHashSet<u32> =
            FxHashSet::with_capacity_and_hasher(32, Default::default());

        // Get direct dependencies from dep map
        if let Some(direct_deps) = dep.get(key) {
            for dep_name in direct_deps.iter() {
                // Resolve dep_name to pu_key indices via indexed_tags
                if let Some(parsed_values) = indexed_tags.get(dep_name) {
                    for &(pu_type, dep_idx) in parsed_values {
                        if dep_idx != key_idx && !transitive_deps.contains(&dep_idx) {
                            // Handle enumerator -> parent enum dependency
                            if pu_type == PuType::Enumerator {
                                if let Some(parent_idx) = indexed_enum.get(dep_name) {
                                    transitive_deps.insert(parent_idx);
                                }
                            }

                            transitive_deps.insert(dep_idx);

                            // Recursively compute child first
                            Self::compute_transitive_for_key_indexed(
                                dep_idx, dep, indexed_tags, indexed_enum, key_index, visiting, memo
                            );

                            // Extend from memo reference (no string cloning!)
                            if let Some(child_deps) = memo.get(&dep_idx) {
                                transitive_deps.extend(child_deps.iter().copied());
                            }
                        }
                    }
                }
            }
        }

        visiting.remove(&key_idx);
        memo.insert(key_idx, transitive_deps);
    }
}

/// Processing Unit type enum - faster than string comparisons
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum PuType {
    Function = 0,
    Variable = 1,
    ExternVar = 2,
    Typedef = 3,
    Enum = 4,
    Struct = 5,
    Union = 6,
    Enumerator = 7,
    Prototype = 8,
    Member = 9,
    Unknown = 255,
}

impl PuType {
    /// Convert from numeric ID (from C FFI) - O(1) direct mapping
    #[inline]
    pub const fn from_id(id: u8) -> Self {
        match id {
            0 => PuType::Function,
            1 => PuType::Variable,
            2 => PuType::ExternVar,
            3 => PuType::Typedef,
            4 => PuType::Enum,
            5 => PuType::Struct,
            6 => PuType::Union,
            7 => PuType::Enumerator,
            8 => PuType::Prototype,
            9 => PuType::Member,
            _ => PuType::Unknown,
        }
    }

    /// Parse type string to enum - O(1) average with first-char dispatch
    #[inline]
    pub fn from_str(s: &str) -> Self {
        // Fast dispatch based on first character
        match s.as_bytes().first() {
            Some(b'f') => {
                if s == "function" || s == "fn" { PuType::Function }
                else { PuType::Unknown }
            }
            Some(b'v') => {
                if s == "variable" { PuType::Variable }
                else { PuType::Unknown }
            }
            Some(b'e') => {
                if s == "externvar" { PuType::ExternVar }
                else if s == "enum" { PuType::Enum }
                else if s == "enumerator" { PuType::Enumerator }
                else { PuType::Unknown }
            }
            Some(b't') => {
                if s == "typedef" { PuType::Typedef }
                else { PuType::Unknown }
            }
            Some(b's') => {
                if s == "struct" { PuType::Struct }
                else { PuType::Unknown }
            }
            Some(b'u') => {
                if s == "union" { PuType::Union }
                else { PuType::Unknown }
            }
            Some(b'p') => {
                if s == "prototype" { PuType::Prototype }
                else { PuType::Unknown }
            }
            Some(b'm') => {
                if s == "member" { PuType::Member }
                else { PuType::Unknown }
            }
            _ => PuType::Unknown,
        }
    }

    /// Convert to string slice for key building
    #[inline]
    pub const fn as_str(&self) -> &'static str {
        match self {
            PuType::Function => "function",
            PuType::Variable => "variable",
            PuType::ExternVar => "externvar",
            PuType::Typedef => "typedef",
            PuType::Enum => "enum",
            PuType::Struct => "struct",
            PuType::Union => "union",
            PuType::Enumerator => "enumerator",
            PuType::Prototype => "prototype",
            PuType::Member => "member",
            PuType::Unknown => "unknown",
        }
    }

    /// Check if this is a "primary" type that gets a UID in split mode
    #[inline]
    pub const fn is_split_primary(&self) -> bool {
        matches!(self, PuType::Function)
    }

    /// Check if this type should be included in no-split mode dependency tracking
    #[inline]
    pub const fn is_nosplit_tracked(&self) -> bool {
        matches!(self,
            PuType::Function | PuType::Variable | PuType::ExternVar |
            PuType::Typedef | PuType::Enum | PuType::Struct | PuType::Union
        )
    }

    /// Check if this is a declaration type (for common header extraction)
    #[inline]
    pub const fn is_declaration(&self) -> bool {
        matches!(self,
            PuType::Typedef | PuType::Enum | PuType::Struct |
            PuType::Union | PuType::ExternVar
        )
    }

    /// Check if this is a variable type (variable or externvar)
    #[inline]
    pub const fn is_variable(&self) -> bool {
        matches!(self, PuType::Variable | PuType::ExternVar)
    }

    /// Check if this is a function type
    #[inline]
    pub const fn is_function(&self) -> bool {
        matches!(self, PuType::Function)
    }

    /// Extract PuType from a full key like "function:name:file" - O(1) byte-level dispatch
    /// Uses direct byte indexing instead of starts_with() for maximum speed.
    /// Each type has a unique (first_byte, colon_position) pair that we check.
    #[inline]
    pub fn from_key(key: &str) -> Self {
        let bytes = key.as_bytes();
        // Fast dispatch based on first byte, then verify colon position
        match bytes.first() {
            Some(b'f') => {
                // "function:" = 9 chars, colon at byte 8
                if bytes.len() > 9 && bytes[8] == b':' {
                    PuType::Function
                } else {
                    PuType::Unknown
                }
            }
            Some(b'v') => {
                // "variable:" = 9 chars, colon at byte 8
                if bytes.len() > 9 && bytes[8] == b':' {
                    PuType::Variable
                } else {
                    PuType::Unknown
                }
            }
            Some(b'e') => {
                // Distinguish: externvar: (10), enum: (5), enumerator: (11)
                // Check byte 1: 'x' for externvar, 'n' for enum/enumerator
                match bytes.get(1) {
                    Some(b'x') => {
                        // "externvar:" = 10 chars, colon at byte 9
                        if bytes.len() > 10 && bytes[9] == b':' {
                            PuType::ExternVar
                        } else {
                            PuType::Unknown
                        }
                    }
                    Some(b'n') => {
                        // "enum:" or "enumerator:" - check byte 4: ':' vs 'e'
                        match bytes.get(4) {
                            Some(b':') => PuType::Enum,
                            Some(b'e') => {
                                // "enumerator:" = 11 chars, colon at byte 10
                                if bytes.len() > 11 && bytes[10] == b':' {
                                    PuType::Enumerator
                                } else {
                                    PuType::Unknown
                                }
                            }
                            _ => PuType::Unknown,
                        }
                    }
                    _ => PuType::Unknown,
                }
            }
            Some(b't') => {
                // "typedef:" = 8 chars, colon at byte 7
                if bytes.len() > 8 && bytes[7] == b':' {
                    PuType::Typedef
                } else {
                    PuType::Unknown
                }
            }
            Some(b's') => {
                // "struct:" = 7 chars, colon at byte 6
                if bytes.len() > 7 && bytes[6] == b':' {
                    PuType::Struct
                } else {
                    PuType::Unknown
                }
            }
            Some(b'u') => {
                // "union:" = 6 chars, colon at byte 5
                if bytes.len() > 6 && bytes[5] == b':' {
                    PuType::Union
                } else {
                    PuType::Unknown
                }
            }
            Some(b'p') => {
                // "prototype:" = 10 chars, colon at byte 9
                if bytes.len() > 10 && bytes[9] == b':' {
                    PuType::Prototype
                } else {
                    PuType::Unknown
                }
            }
            Some(b'm') => {
                // "member:" = 7 chars, colon at byte 6
                if bytes.len() > 7 && bytes[6] == b':' {
                    PuType::Member
                } else {
                    PuType::Unknown
                }
            }
            _ => PuType::Unknown,
        }
    }

    /// Check if key represents a function or prototype type
    #[inline]
    pub fn key_is_func_or_proto(key: &str) -> bool {
        let bytes = key.as_bytes();
        match bytes.first() {
            // "function:" = 9 chars, colon at byte 8
            Some(b'f') => bytes.len() > 9 && bytes[8] == b':',
            // "prototype:" = 10 chars, colon at byte 9
            Some(b'p') => bytes.len() > 10 && bytes[9] == b':',
            _ => false,
        }
    }

    /// Check if key represents a variable type (variable or externvar)
    #[inline]
    pub fn key_is_variable(key: &str) -> bool {
        let bytes = key.as_bytes();
        match bytes.first() {
            // "variable:" = 9 chars, colon at byte 8
            Some(b'v') => bytes.len() > 9 && bytes[8] == b':',
            // "externvar:" = 10 chars, colon at byte 9, byte 1 must be 'x'
            Some(b'e') => bytes.len() > 10 && bytes.get(1) == Some(&b'x') && bytes[9] == b':',
            _ => false,
        }
    }

    /// Check if key represents function, variable, or externvar
    #[inline]
    pub fn key_is_func_or_var(key: &str) -> bool {
        let bytes = key.as_bytes();
        match bytes.first() {
            // "function:" = 9 chars, colon at byte 8
            Some(b'f') => bytes.len() > 9 && bytes[8] == b':',
            // "variable:" = 9 chars, colon at byte 8
            Some(b'v') => bytes.len() > 9 && bytes[8] == b':',
            // "externvar:" = 10 chars, colon at byte 9, byte 1 must be 'x'
            Some(b'e') => bytes.len() > 10 && bytes.get(1) == Some(&b'x') && bytes[9] == b':',
            _ => false,
        }
    }

    /// Check if key represents a type definition (typedef, struct, union, enum, enumerator, variable, externvar)
    #[inline]
    pub fn key_is_type_def(key: &str) -> bool {
        let bytes = key.as_bytes();
        match bytes.first() {
            // "typedef:" = 8 chars, colon at byte 7
            Some(b't') => bytes.len() > 8 && bytes[7] == b':',
            // "struct:" = 7 chars, colon at byte 6
            Some(b's') => bytes.len() > 7 && bytes[6] == b':',
            // "union:" = 6 chars, colon at byte 5
            Some(b'u') => bytes.len() > 6 && bytes[5] == b':',
            // For 'e': enum (5), enumerator (11), externvar (10)
            Some(b'e') => {
                match bytes.get(1) {
                    Some(b'x') => bytes.len() > 10 && bytes[9] == b':', // externvar
                    Some(b'n') => {
                        // enum or enumerator - check byte 4
                        bytes.get(4) == Some(&b':') || // enum:
                        (bytes.len() > 11 && bytes[10] == b':') // enumerator:
                    }
                    _ => false,
                }
            }
            // "variable:" = 9 chars, colon at byte 8
            Some(b'v') => bytes.len() > 9 && bytes[8] == b':',
            _ => false,
        }
    }

    /// Check if this is a prototype type
    #[inline]
    pub const fn is_prototype(&self) -> bool {
        matches!(self, PuType::Prototype)
    }

    /// Check if this is function or prototype
    #[inline]
    pub const fn is_function_or_prototype(&self) -> bool {
        matches!(self, PuType::Function | PuType::Prototype)
    }

    /// Check if this is variable or externvar
    #[inline]
    pub const fn is_var_or_externvar(&self) -> bool {
        matches!(self, PuType::Variable | PuType::ExternVar)
    }

    /// Check if this is an externvar
    #[inline]
    pub const fn is_externvar(&self) -> bool {
        matches!(self, PuType::ExternVar)
    }
}

/// Unit key: "type:name:file" format used throughout the codebase
/// These helpers centralize key creation/parsing for consistency

/// Create a unit key from type string and components
#[inline]
fn make_unit_key(type_str: &str, name: &str, file: &str) -> String {
    let capacity = type_str.len() + name.len() + file.len() + 2;
    let mut key = String::with_capacity(capacity);
    key.push_str(type_str);
    key.push(':');
    key.push_str(name);
    key.push(':');
    key.push_str(file);
    key
}

/// Parse a full key "type:name:file" into its components without allocation
/// Returns (type, name, file) or None if parsing fails
/// Uses memchr for faster colon finding
#[inline]
fn parse_key_parts(key: &str) -> Option<(&str, &str, &str)> {
    let bytes = key.as_bytes();
    let first_colon = memchr::memchr(b':', bytes)?;
    let rest = &bytes[first_colon + 1..];
    let second_colon = memchr::memchr(b':', rest)?;
    Some((
        &key[..first_colon],
        // Safe: key is already a valid UTF-8 str
        std::str::from_utf8(&rest[..second_colon]).unwrap(),
        std::str::from_utf8(&rest[second_colon + 1..]).unwrap(),
    ))
}

/// Parse a key into (type, rest) without allocation
/// For "type:name:file" returns ("type", "name:file")
/// For "type:file" returns ("type", "file")
/// Uses memchr for faster colon finding
#[inline]
fn parse_key_type_rest(key: &str) -> Option<(&str, &str)> {
    let bytes = key.as_bytes();
    let colon = memchr::memchr(b':', bytes)?;
    Some((&key[..colon], &key[colon + 1..]))
}

/// Extract the name from a full key "type:name:file" without allocation
/// Uses memchr for faster colon finding
#[inline]
fn extract_key_name(key: &str) -> Option<&str> {
    let bytes = key.as_bytes();
    let first_colon = memchr::memchr(b':', bytes)?;
    let rest = &bytes[first_colon + 1..];
    let second_colon = memchr::memchr(b':', rest)?;
    // Safe: key is already a valid UTF-8 str
    Some(std::str::from_utf8(&rest[..second_colon]).unwrap())
}

/// Extract the file part from a key "type:name:file" or "type:file" without allocation
/// Uses memrchr for faster reverse colon finding
#[inline]
#[allow(dead_code)]
fn extract_key_file(key: &str) -> Option<&str> {
    let bytes = key.as_bytes();
    memchr::memrchr(b':', bytes).map(|pos| &key[pos + 1..])
}

// Profiling counters
static PROFILE_ENABLED: AtomicBool = AtomicBool::new(false);
static DEPENDS_ON_COUNT: AtomicU64 = AtomicU64::new(0);
static DEPENDS_ON_TIME_NS: AtomicU64 = AtomicU64::new(0);
static PUTCHAR_COUNT: AtomicU64 = AtomicU64::new(0);
static DEBUG_ENTRY_COUNT: AtomicU64 = AtomicU64::new(0);
static DEBUG_ENTRY_TIME_NS: AtomicU64 = AtomicU64::new(0);
static BUFFER_BYTES_COPIED: AtomicU64 = AtomicU64::new(0);
static BUFFER_COPY_COUNT: AtomicU64 = AtomicU64::new(0);
static PROCESS_ENTRY_TIME_NS: AtomicU64 = AtomicU64::new(0);
static PROCESS_ENTRY_COUNT: AtomicU64 = AtomicU64::new(0);

// use_dependency profiling counters
static USE_DEP_TRANS_NS: AtomicU64 = AtomicU64::new(0);
static USE_DEP_PROTO_SCAN_NS: AtomicU64 = AtomicU64::new(0);
static USE_DEP_TYPEDEF_SCAN_NS: AtomicU64 = AtomicU64::new(0);
static USE_DEP_FIXPOINT_NS: AtomicU64 = AtomicU64::new(0);
static USE_DEP_PRINT_NS: AtomicU64 = AtomicU64::new(0);
static USE_DEP_COUNT: AtomicU64 = AtomicU64::new(0);

// Cached env var lookups - avoid repeated syscalls in hot paths
static DEBUG_TAGS_ENABLED: AtomicBool = AtomicBool::new(false);

fn init_debug_tags() {
    DEBUG_TAGS_ENABLED.store(std::env::var("DEBUG_TAGS").is_ok(), Ordering::Relaxed);
}

pub fn enable_profiling() {
    PROFILE_ENABLED.store(true, Ordering::Relaxed);
}

pub fn print_profile_stats() {
    if PROFILE_ENABLED.load(Ordering::Relaxed) {
        eprintln!("RUST PROFILE depends_on: {} calls, {:.3} ms total",
            DEPENDS_ON_COUNT.load(Ordering::Relaxed),
            DEPENDS_ON_TIME_NS.load(Ordering::Relaxed) as f64 / 1_000_000.0);
        eprintln!("RUST PROFILE putchar: {} calls",
            PUTCHAR_COUNT.load(Ordering::Relaxed));
        eprintln!("RUST PROFILE debugEntry: {} calls, {:.3} ms total",
            DEBUG_ENTRY_COUNT.load(Ordering::Relaxed),
            DEBUG_ENTRY_TIME_NS.load(Ordering::Relaxed) as f64 / 1_000_000.0);
        let buffer_copies = BUFFER_COPY_COUNT.load(Ordering::Relaxed);
        let buffer_bytes = BUFFER_BYTES_COPIED.load(Ordering::Relaxed);
        eprintln!("RUST PROFILE buffer_copy: {} copies, {} KB total ({} bytes/copy avg)",
            buffer_copies,
            buffer_bytes / 1024,
            if buffer_copies > 0 { buffer_bytes / buffer_copies } else { 0 });
        eprintln!("RUST PROFILE process_entry: {} calls, {:.3} ms total",
            PROCESS_ENTRY_COUNT.load(Ordering::Relaxed),
            PROCESS_ENTRY_TIME_NS.load(Ordering::Relaxed) as f64 / 1_000_000.0);
        // use_dependency breakdown
        let use_dep_count = USE_DEP_COUNT.load(Ordering::Relaxed);
        if use_dep_count > 0 {
            eprintln!("RUST PROFILE use_dependency: {} calls", use_dep_count);
            eprintln!("  transitive_deps: {:.3} ms", USE_DEP_TRANS_NS.load(Ordering::Relaxed) as f64 / 1_000_000.0);
            eprintln!("  proto_scan: {:.3} ms", USE_DEP_PROTO_SCAN_NS.load(Ordering::Relaxed) as f64 / 1_000_000.0);
            eprintln!("  typedef_scan: {:.3} ms", USE_DEP_TYPEDEF_SCAN_NS.load(Ordering::Relaxed) as f64 / 1_000_000.0);
            eprintln!("  fixpoint: {:.3} ms", USE_DEP_FIXPOINT_NS.load(Ordering::Relaxed) as f64 / 1_000_000.0);
            eprintln!("  print: {:.3} ms", USE_DEP_PRINT_NS.load(Ordering::Relaxed) as f64 / 1_000_000.0);
        }
    }
}

thread_local! {
    static INPUT_BUFFER: RefCell<Vec<u8>> = RefCell::new(Vec::new());
}

mod ffi;
use ffi::DCTags;

// Public modules for end-to-end build process
pub mod preprocessor;
pub mod project;
pub mod worker_pool;
pub mod crossfile;
pub mod experiment_db;

// Re-export key types for convenience
pub use preprocessor::{Preprocessor, PreprocessorConfig, ProjectConfig, ProjectType};
pub use crossfile::{CrossFileDeps, FileAnalysis, FileAnalysisBuilder, Symbol, SymbolKind, CrossFileStats};
pub use project::{
    ProjectPrecompiler, PrecompileOptions, PrecompileResult, ProjectPrecompileResult,
    TimingStats, precompile_vim, precompile_file, precompile_file_with_config,
};

// Consolidated postponed state - single lock instead of 5 separate ones
#[derive(Default, Clone)]
struct PostponedState {
    kind: Option<String>,
    name: Option<String>,
    file: Option<String>,
    scope_kind: Option<String>,
    scope_name: Option<String>,
}

impl PostponedState {
    fn clear(&mut self) {
        self.kind = None;
        self.name = None;
        self.file = None;
        self.scope_kind = None;
        self.scope_name = None;
    }

    fn is_struct_or_union(&self) -> bool {
        matches!(self.kind.as_deref(), Some("struct") | Some("union"))
    }
}

// Single consolidated state structure - eliminates extra mutex acquisitions
static TAG_INFO: Lazy<Arc<Mutex<TagInfo>>> = Lazy::new(|| Arc::new(Mutex::new(TagInfo::default())));

// ============================================================================
// Thread-Local TagInfo for Multi-File Parallel Mode
// ============================================================================
// When processing multiple files in parallel (in-process), each thread maintains
// its own TagInfo. This avoids global mutex contention and enables true parallelism.
// The USE_PARALLEL_MODE flag determines which TagInfo to use in callbacks.

thread_local! {
    /// Flag indicating whether this thread is in parallel processing mode
    static USE_PARALLEL_MODE: RefCell<bool> = RefCell::new(false);

    /// Thread-local TagInfo for parallel mode
    static THREAD_TAG_INFO: RefCell<TagInfo> = RefCell::new(TagInfo::default());
}

/// Set parallel mode for the current thread
fn set_parallel_mode(enabled: bool) {
    USE_PARALLEL_MODE.with(|m| *m.borrow_mut() = enabled);
}

/// Check if current thread is in parallel mode
fn is_parallel_mode() -> bool {
    USE_PARALLEL_MODE.with(|m| *m.borrow())
}

/// Execute a closure with exclusive access to the appropriate TagInfo.
/// In parallel mode, uses thread-local TagInfo (no mutex contention).
/// In single-file mode, uses global TAG_INFO (existing behavior).
#[inline]
fn with_tag_info<F, R>(f: F) -> R
where
    F: FnOnce(&mut TagInfo) -> R,
{
    if is_parallel_mode() {
        THREAD_TAG_INFO.with(|ti| f(&mut ti.borrow_mut()))
    } else {
        f(&mut TAG_INFO.lock())
    }
}

/// Reset thread-local TagInfo for processing a new file
fn reset_thread_tag_info() {
    THREAD_TAG_INFO.with(|ti| {
        *ti.borrow_mut() = TagInfo::default();
    });
}

/// Take the thread-local TagInfo (for returning results from parallel processing)
#[allow(dead_code)]
fn take_thread_tag_info() -> TagInfo {
    THREAD_TAG_INFO.with(|ti| std::mem::take(&mut *ti.borrow_mut()))
}

struct TagInfo {
    // Postponed state (was separate POSTPONED mutex)
    postponed: PostponedState,

    // Main tag processing state
    lines: String,
    pu: FxHashMap<String, String>,
    pu_order: Vec<String>,
    pu_order_set: FxHashSet<String>,  // O(1) lookup companion for pu_order
    dep: FxHashMap<String, Vec<String>>,
    tags: FxHashMap<String, Vec<String>>,
    to_dep: Vec<String>,
    headlines: String,
    // Maps enumerator name to parent enum unit key (for anonymous enum handling)
    enumerator_to_enum: FxHashMap<String, String>,
    // System typedefs extracted from preprocessed headers (for split mode)
    // Each entry is (typedef_name, full_typedef_line)
    system_typedefs: Vec<(String, String)>,
    // Tracks anonymous enum names (like __anon369) to their unit keys
    // This allows us to properly handle enumerators from anonymous enums
    anon_enum_units: FxHashMap<String, String>,
    // Tracks the last variable with a proper type declaration (for comma-separated handling)
    // Format: (variable_unit_key, variable_name) - used to link comma-continuations
    last_typed_variable: Option<(String, String)>,
    // Extern function declarations extracted from preprocessed file (for split mode)
    // These are system functions like close(), read(), write() that ctags doesn't capture
    // Map: function_name -> full_declaration
    extern_functions: FxHashMap<String, String>,
    // Bug48: Extern variable declarations extracted from preprocessed file (for split mode)
    // These are extern const struct declarations like wl_callback_interface that ctags doesn't capture
    // Map: variable_name -> full_declaration
    extern_variables: FxHashMap<String, String>,
    // Bug35: Tracks incomplete typedef for multi-name typedef merging
    // When a typedef like "typedef T *A, *B;" is processed:
    // - ctags emits "A" with code "typedef T *A" (missing semicolon)
    // - ctags emits "B" with code ", *B;" (starts with comma)
    // We need to merge these into a single complete typedef
    // Stores: (primary_typedef_unit_key, file) when a typedef doesn't end with semicolon
    incomplete_typedef: Option<(String, String)>,
    // Bug71: Static function pointer variable declarations that ctags doesn't capture
    // Pattern: static <type> *((*name)(<params>));
    // Example: static char_u *((*set_opt_callback_func)(expand_T *, int));
    // Map: variable_name -> full_declaration
    static_funcptr_vars: FxHashMap<String, String>,
    // Line numbers reported by ctags for each unit key (for body extraction)
    // Map: unit_key -> 1-based source line number
    line_numbers: FxHashMap<String, u64>,
}

impl Default for TagInfo {
    fn default() -> Self {
        // Pre-allocate capacity for expected number of entries
        // Based on profiling: ~5000 unique entries, ~40000 dependencies
        TagInfo {
            postponed: PostponedState::default(),
            lines: String::with_capacity(4096),  // Typical function body
            pu: FxHashMap::with_capacity_and_hasher(8192, Default::default()),
            pu_order: Vec::with_capacity(8192),
            pu_order_set: FxHashSet::with_capacity_and_hasher(8192, Default::default()),
            dep: FxHashMap::with_capacity_and_hasher(4096, Default::default()),
            tags: FxHashMap::with_capacity_and_hasher(2048, Default::default()),
            to_dep: Vec::with_capacity(64),  // Per-entry dependencies, cleared often
            headlines: String::with_capacity(256),
            enumerator_to_enum: FxHashMap::with_capacity_and_hasher(2048, Default::default()),
            system_typedefs: Vec::with_capacity(512),
            anon_enum_units: FxHashMap::with_capacity_and_hasher(128, Default::default()),
            last_typed_variable: None,
            extern_functions: FxHashMap::default(),
            extern_variables: FxHashMap::default(),
            incomplete_typedef: None,
            static_funcptr_vars: FxHashMap::default(),
            line_numbers: FxHashMap::default(),
        }
    }
}

/// Extract extern function declarations from a preprocessed file.
/// These are system functions like close(), read(), write() that ctags doesn't capture.
/// Returns a HashMap of function_name -> full_declaration.
/// Single-pass file scan computing brace count, source fraction, and incomplete-file flag.
///
/// Returns `(brace_count, src_frac, is_incomplete)`:
/// - `brace_count`: source-file-only `{` lines preceded by `)` lines (proxy for PU count).
///   Only counts braces from the primary source file; not headers. r=0.996 vs actual PU count.
/// - `src_frac`: fraction of non-empty, non-directive lines from the primary source file.
///   Low (< 2%) = header-dominated; high (≥ 5%) = source-dominated.
/// - `is_incomplete`: true for vim Windows/GUI/test files, or `char_u` files without definition.
fn scan_file_properties(filename: &str) -> (usize, f64, bool) {
    use std::io::{BufRead, BufReader};
    let file = match std::fs::File::open(filename) {
        Ok(f) => f,
        Err(_) => return (0, 0.0, false),
    };
    let reader = BufReader::with_capacity(65536, file);

    // --- brace-count state ---
    let mut brace_count = 0usize;
    let mut prev_ends_with_paren = false;
    let mut primary_file: Option<String> = None;
    let mut in_primary = true;
    let mut src_lines = 0usize;
    let mut hdr_lines = 0usize;

    // --- incomplete-file state ---
    let mut typedef_count = 0u32;
    let mut uses_char_u = false;
    let mut defines_char_u = false;
    let mut is_win_source = false;
    let mut is_gui_source = false;
    let mut line_count = 0u32;
    // incomplete_settled: true once we know the answer (early-exit shortcut)
    let mut incomplete_settled = false;

    // Win/GUI/test source file patterns (basename substrings in #line markers)
    let win_patterns  = ["winclip.c", "os_w32exe.c", "os_win32.c", "gui_w32.c", "if_ole.c"];
    let gui_patterns  = ["gui_photon.c", "gui_x11.c", "gui_xim.c", "gui_gtk.c", "gui_motif.c"];
    let test_patterns = ["_test.c"];

    for line_result in reader.lines() {
        let line = match line_result {
            Ok(l) => l,
            Err(_) => break,
        };
        let trimmed = line.trim();

        // ── #line directive handling ──────────────────────────────────────────
        if trimmed.starts_with('#') {
            let rest = trimmed[1..].trim_start();
            if rest.starts_with(|c: char| c.is_ascii_digit()) {
                // Parse quoted filename from `# <lineno> "fname" [flags]`
                if let Some(q_start) = rest.find('"') {
                    let after_quote = &rest[q_start + 1..];
                    if let Some(q_end) = after_quote.find('"') {
                        let fname = &after_quote[..q_end];
                        if fname != "<built-in>" && fname != "<command-line>" {
                            // brace-count: track primary source file
                            if primary_file.is_none() {
                                primary_file = Some(fname.to_string());
                                in_primary = true;
                            } else {
                                in_primary = primary_file.as_deref() == Some(fname);
                            }
                            // incomplete-check: check for win/gui/test source paths
                            if !incomplete_settled && (fname.ends_with(".c") || fname.ends_with(".cpp")) {
                                for p in &win_patterns  { if fname.contains(p) { is_win_source = true; } }
                                for p in &gui_patterns  { if fname.contains(p) { is_gui_source = true; } }
                                for p in &test_patterns { if fname.contains(p) { is_gui_source = true; } }
                            }
                        }
                    }
                }
                prev_ends_with_paren = false;
                line_count += 1;
                continue;
            }
        }

        if trimmed.is_empty() {
            continue;
        }

        line_count += 1;

        // ── source-fraction line counting ─────────────────────────────────────
        if in_primary { src_lines += 1; } else { hdr_lines += 1; }

        // ── brace-count logic ─────────────────────────────────────────────────
        // Count function-opening braces in both Allman and K&R styles:
        //   Allman: `{` alone on its own line, preceded by a line ending `)`
        //   K&R:    line ends with `) {` or `){` (opening brace on same line as params)
        if in_primary {
            let ends_with_open = trimmed.ends_with(") {")
                || trimmed.ends_with("){")
                || trimmed.ends_with(") __attribute__((noinline)) {")  // gcc attrs
                || (trimmed == "{" && prev_ends_with_paren);           // Allman
            if ends_with_open {
                brace_count += 1;
            }
        }
        prev_ends_with_paren = in_primary && (trimmed.ends_with(')')
            || trimmed.ends_with("__attribute__((noinline))")
            || trimmed.ends_with("__attribute__((cold))")
            || trimmed.ends_with("__attribute__((hot))"));

        // ── incomplete-file logic (only until settled) ────────────────────────
        if !incomplete_settled {
            if trimmed.contains("typedef ") {
                typedef_count += 1;
                if trimmed.contains("char_u") {
                    defines_char_u = true;
                }
                // Once we have 100 typedefs and no platform flags, we know it's complete —
            // stop scanning incomplete-state (brace counting continues in outer loop).
                if typedef_count > 100 && !is_win_source && !is_gui_source {
                    incomplete_settled = true;
                }
            } else if !defines_char_u
                && (trimmed.contains("char_u ") || trimmed.contains("char_u*") || trimmed.contains("char_u\t"))
            {
                uses_char_u = true;
            }

            // Don't scan incomplete-check state past first 10K lines
            if line_count > 10_000 {
                incomplete_settled = true;
            }
        }
    }

    // Determine is_incomplete result.
    // Note: incomplete_settled just means "stop scanning" — it does NOT mean "is complete".
    // A file with is_win_source/is_gui_source is still incomplete even if typedef_count > 100.
    #[cfg(not(target_os = "windows"))]
    let platform_flag = is_win_source || is_gui_source;
    #[cfg(target_os = "windows")]
    let platform_flag = is_gui_source;

    let is_incomplete = if platform_flag {
        true
    } else if typedef_count > 100 {
        // Many typedefs and no platform flag → complete file
        false
    } else {
        uses_char_u && !defines_char_u && typedef_count < 50
    };

    let total = src_lines + hdr_lines;
    let src_frac = if total > 0 { src_lines as f64 / total as f64 } else { 0.0 };

    (brace_count, src_frac, is_incomplete)
}

// ============================================================================
// Strategy Analysis - recommend compilation strategy for a set of .i files
// ============================================================================

/// Full scan of a single `.i` file for strategy analysis.
/// Returns `(src_fraction, fn_brace_count, header_count)`.
///
/// - `src_fraction`: fraction of non-empty, non-directive lines that come from
///   the primary source file (not included headers).
/// - `fn_brace_count`: source-file-only function-brace count (proxy for PU count).
/// - `header_count`: number of distinct headers included by this file.
fn analyze_single_file(filename: &str) -> (f64, usize, usize) {
    use std::io::{BufRead, BufReader};
    use std::collections::HashSet;

    let file = match std::fs::File::open(filename) {
        Ok(f) => f,
        Err(_) => return (0.0, 0, 0),
    };

    // Determine primary file from first `# N "path"` directive
    let mut reader = BufReader::with_capacity(65536, file);
    let mut primary_file = String::new();
    {
        let mut line_buf = String::new();
        loop {
            line_buf.clear();
            match reader.read_line(&mut line_buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            let trimmed = line_buf.trim_start();
            if trimmed.starts_with('#') {
                let rest = trimmed[1..].trim_start();
                if rest.starts_with(|c: char| c.is_ascii_digit()) {
                    if let Some(start) = rest.find('"') {
                        if let Some(end) = rest[start + 1..].find('"') {
                            let path = &rest[start + 1..start + 1 + end];
                            if path != "<built-in>" && path != "<command-line>" {
                                primary_file = path.to_string();
                                break;
                            }
                        }
                    }
                }
            }
        }
        // Re-open for full scan (BufReader doesn't support seek easily)
    }

    // Re-open and do the full scan
    let file2 = match std::fs::File::open(filename) {
        Ok(f) => f,
        Err(_) => return (0.0, 0, 0),
    };
    let reader2 = BufReader::with_capacity(65536, file2);

    let mut src_lines = 0usize;
    let mut hdr_lines = 0usize;
    let mut brace_count = 0usize;
    let mut prev_ends_with_paren = false;
    let mut in_primary = true;
    let mut headers: HashSet<String> = HashSet::new();

    for line_result in reader2.lines() {
        let line = match line_result {
            Ok(l) => l,
            Err(_) => break,
        };
        let trimmed = line.trim_start();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with('#') {
            let rest = trimmed[1..].trim_start();
            if rest.starts_with(|c: char| c.is_ascii_digit()) {
                if let Some(start) = rest.find('"') {
                    if let Some(end) = rest[start + 1..].find('"') {
                        let path = &rest[start + 1..start + 1 + end];
                        if path != "<built-in>" && path != "<command-line>" {
                            in_primary = path == primary_file;
                            if !in_primary {
                                headers.insert(path.to_string());
                            }
                        }
                    }
                }
                prev_ends_with_paren = false;
                continue;
            }
        }
        if in_primary {
            src_lines += 1;
            // Match both Allman style (`{` alone after `)`) and K&R style (`) {` on same line).
            let ends_with_open = trimmed.ends_with(") {")
                || trimmed.ends_with("){")
                || (trimmed == "{" && prev_ends_with_paren);
            if ends_with_open {
                brace_count += 1;
            }
            prev_ends_with_paren = trimmed.ends_with(')');
        } else {
            hdr_lines += 1;
            prev_ends_with_paren = false;
        }
    }

    let total = src_lines + hdr_lines;
    let src_frac = if total > 0 { src_lines as f64 / total as f64 } else { 0.0 };
    (src_frac, brace_count, headers.len())
}

/// Recommended compilation strategy for a set of `.i` files.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BuildStrategy {
    /// Precompiled headers — header-dominated files where PCH amortises well.
    Pch,
    /// Function-level splitting — source-rich files with many functions.
    Split,
    /// Pass through as-is — small files where neither PCH nor splitting helps.
    Passthrough,
}

impl std::fmt::Display for BuildStrategy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BuildStrategy::Pch => write!(f, "pch"),
            BuildStrategy::Split => write!(f, "split"),
            BuildStrategy::Passthrough => write!(f, "passthrough"),
        }
    }
}

/// Build-type detected from presence of up-to-date `.h.gch` files.
#[derive(Debug, Clone, PartialEq)]
pub enum BuildType {
    Fresh,
    Incremental,
}

impl std::fmt::Display for BuildType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BuildType::Fresh => write!(f, "fresh"),
            BuildType::Incremental => write!(f, "incremental"),
        }
    }
}

/// Per-file strategy entry (for `--per-file` output).
pub struct FileStrategyEntry {
    pub path: String,
    pub strategy: BuildStrategy,
    pub src_frac: f64,
    pub fn_braces: usize,
}

/// Full strategy analysis result.
pub struct StrategyResult {
    pub strategy: BuildStrategy,
    pub build_type: BuildType,
    pub stale_frac: f64,
    pub mean_src_frac: f64,
    pub mean_fn_braces: f64,
    pub n_files: usize,
    pub reason: String,
    /// Per-file breakdown (populated when `per_file=true`).
    pub per_file: Vec<FileStrategyEntry>,
}

// ===== Config I/O =====
// Two-phase compilation: `precc profile <dir>` writes .precc-config.toml once;
// subsequent builds load it to skip per-file scanning.

/// Per-file entry stored in `.precc-config.toml`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PreccFileEntry {
    /// Bare filename (e.g. `"eval.i"`), no directory prefix.
    pub path: String,
    /// Recommended strategy for this file.
    pub strategy: BuildStrategy,
    /// Fraction of non-header lines (0.0–1.0).
    pub src_frac: f64,
    /// Estimated number of top-level function bodies.
    pub fn_braces: usize,
    /// Unix timestamp (seconds) of the `.i` file when this entry was written.
    /// Used for staleness detection: if the file is newer, re-scan.
    pub mtime: u64,
}

/// Project-level config, serialised to/from `.precc-config.toml`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PreccConfig {
    pub version: String,
    pub generated_at: String,
    pub overall_strategy: BuildStrategy,
    pub build_type: String,
    pub jobs: usize,
    pub mean_src_frac: f64,
    pub mean_fn_braces: f64,
    pub n_files: usize,
    /// ML hook: path to a binary/script that accepts a `.i` filepath and writes
    /// "pch", "split", or "passthrough" to stdout.  Leave `None` to disable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ml_model_path: Option<String>,
    pub files: Vec<PreccFileEntry>,
}

/// Return the Unix-epoch-seconds mtime of `path`, or 0 on any error.
fn file_mtime_secs(path: &str) -> u64 {
    file_mtime_secs_pub(path)
}

/// Public alias for [`file_mtime_secs`] — used by the `main.rs` profile command.
pub fn file_mtime_secs_pub(path: &str) -> u64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .map(|t| t.duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs())
        .unwrap_or(0)
}

/// Load `.precc-config.toml` from `dir`.  Returns `None` if the file is absent
/// or cannot be parsed (silently — the caller falls back to `scan_file_properties`).
pub fn load_config(dir: &std::path::Path) -> Option<PreccConfig> {
    let path = dir.join(".precc-config.toml");
    let text = std::fs::read_to_string(&path).ok()?;
    toml::from_str(&text).ok()
}

/// Convenience wrapper: derive the directory from the `.i` file path and call [`load_config`].
pub fn load_config_for_file(filename: &str) -> Option<PreccConfig> {
    let dir = std::path::Path::new(filename)
        .parent()
        .unwrap_or(std::path::Path::new("."));
    load_config(dir)
}

/// Look up the config entry for a single `.i` file.
/// Returns `None` if:
/// - no entry exists for this file, OR
/// - the entry is stale (the `.i` file's current mtime > stored mtime).
pub fn config_entry_for_file<'a>(cfg: &'a PreccConfig, filename: &str) -> Option<&'a PreccFileEntry> {
    let bare = std::path::Path::new(filename)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(filename);
    let entry = cfg.files.iter().find(|e| e.path == bare)?;
    let current_mtime = file_mtime_secs(filename);
    if current_mtime > entry.mtime {
        return None; // stale — file changed since last profile
    }
    Some(entry)
}

/// Serialise `cfg` to `<dir>/.precc-config.toml`.
pub fn save_config(dir: &std::path::Path, cfg: &PreccConfig) -> std::io::Result<()> {
    use std::io::Write;
    let toml_text = toml::to_string_pretty(cfg)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;
    let path = dir.join(".precc-config.toml");
    let mut f = std::io::BufWriter::new(std::fs::File::create(&path)?);
    writeln!(f, "# precc configuration — generated by `precc profile`")?;
    writeln!(f, "# Edit per-file strategy entries to override automatic decisions.")?;
    writeln!(f, "# ml_model_path: set to a binary that reads a .i path and writes")?;
    writeln!(f, "#   \"pch\", \"split\", or \"passthrough\" to stdout (ML hook, optional).")?;
    writeln!(f)?;
    f.write_all(toml_text.as_bytes())?;
    Ok(())
}

/// Detect whether the build is fresh or incremental by checking whether `.h.gch`
/// files exist and are newer than the corresponding `.i` files.
fn detect_build_type(ifiles: &[&str], outdir: &std::path::Path) -> (BuildType, f64) {
    if ifiles.is_empty() {
        return (BuildType::Fresh, 1.0);
    }
    let stale = ifiles.iter().filter(|f| {
        let stem = std::path::Path::new(f).file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("");
        let gch = outdir.join(format!("{}.h.gch", stem));
        if !gch.exists() {
            return true; // no GCH → stale
        }
        // GCH older than .i → stale
        let gch_mtime = gch.metadata().ok().and_then(|m| m.modified().ok());
        let i_mtime  = std::path::Path::new(f).metadata().ok().and_then(|m| m.modified().ok());
        match (gch_mtime, i_mtime) {
            (Some(g), Some(s)) => g < s,
            _ => true,
        }
    }).count();
    let stale_frac = stale as f64 / ifiles.len() as f64;
    let build_type = if stale_frac > 0.5 { BuildType::Fresh } else { BuildType::Incremental };
    (build_type, stale_frac)
}

/// Analyse a set of `.i` files and recommend a compilation strategy.
///
/// # Arguments
/// - `ifiles`: paths to `.i` preprocessed files (or directories containing them).
/// - `jobs`: parallelism level (`-j` value) used for the recommendation text.
/// - `build_type_override`: `Some("fresh")`, `Some("incremental")`, or `None` for auto-detect.
/// - `outdir`: where `.h.gch` files would live (used for build-type auto-detection).
/// - `per_file`: if true, populate `StrategyResult::per_file` with per-file breakdown.
pub fn analyze_strategy(
    ifiles: &[&str],
    jobs: usize,
    build_type_override: Option<&str>,
    outdir: Option<&std::path::Path>,
    per_file: bool,
) -> StrategyResult {
    use rayon::prelude::*;

    // Expand any directories
    let mut expanded: Vec<String> = Vec::new();
    for f in ifiles {
        let p = std::path::Path::new(f);
        if p.is_dir() {
            if let Ok(entries) = std::fs::read_dir(p) {
                let mut dir_files: Vec<String> = entries
                    .filter_map(|e| e.ok())
                    .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("i"))
                    .map(|e| e.path().to_string_lossy().to_string())
                    .collect();
                dir_files.sort();
                expanded.extend(dir_files);
            }
        } else if p.extension().and_then(|x| x.to_str()) == Some("i") && p.exists() {
            expanded.push(f.to_string());
        }
    }

    if expanded.is_empty() {
        return StrategyResult {
            strategy: BuildStrategy::Passthrough,
            build_type: BuildType::Fresh,
            stale_frac: 1.0,
            mean_src_frac: 0.0,
            mean_fn_braces: 0.0,
            n_files: 0,
            reason: "No .i files found.".to_string(),
            per_file: Vec::new(),
        };
    }

    // Parallel scan
    let results: Vec<(f64, usize, usize)> = expanded
        .par_iter()
        .map(|f| analyze_single_file(f))
        .collect();

    let n = results.len();
    let mean_src = results.iter().map(|r| r.0).sum::<f64>() / n as f64;
    let mean_brace = results.iter().map(|r| r.1 as f64).sum::<f64>() / n as f64;

    // Build-type detection
    let out_path = outdir.map(|p| p.to_path_buf())
        .unwrap_or_else(|| {
            std::path::Path::new(expanded[0].as_str())
                .parent()
                .unwrap_or(std::path::Path::new("."))
                .to_path_buf()
        });
    let expanded_refs: Vec<&str> = expanded.iter().map(|s| s.as_str()).collect();
    let (build_type, stale_frac) = match build_type_override {
        Some("fresh") => (BuildType::Fresh, 1.0),
        Some("incremental") => (BuildType::Incremental, 0.0),
        _ => detect_build_type(&expanded_refs, &out_path),
    };

    // Empirical break-even builds (from measured kernel data):
    //   j1 → 2.7, j8 → 3.7, j24 → 6.9 — rounded up conservatively
    let breakeven: usize = if jobs <= 1 { 3 } else if jobs <= 8 { 4 } else { 7 };

    // --- Decision ---
    let (strategy, reason) = if mean_src < 0.05 {
        let r = if build_type == BuildType::Incremental {
            format!(
                "Header-dominated ({:.1}% source). GCHs cached → src-only compile: −24% (j24) to −46% (j1).",
                mean_src * 100.0
            )
        } else {
            format!(
                "Header-dominated ({:.1}% source, {} files). \
                FRESH BUILD: first-build overhead 2.4× at j{}; profitable after {} builds. \
                Set PRECC_BUILD_TYPE=fresh to force passthrough for one-shot builds.",
                mean_src * 100.0, n, jobs, breakeven
            )
        };
        (BuildStrategy::Pch, r)
    } else if mean_brace > 50.0 {
        let r = format!(
            "Source-dominated ({:.1}% source, avg {:.0} fns/file). \
            Function-level splitting profitable at j≥8.",
            mean_src * 100.0, mean_brace
        );
        (BuildStrategy::Split, r)
    } else {
        let r = format!(
            "Small files (avg {:.0} fns/file, {:.1}% source). Neither PCH nor splitting helps.",
            mean_brace, mean_src * 100.0
        );
        (BuildStrategy::Passthrough, r)
    };

    // Build per-file entries if requested
    let per_file_entries = if per_file {
        let mut entries: Vec<FileStrategyEntry> = expanded.iter().zip(results.iter())
            .map(|(path, (sf, br, _hdr))| {
                let file_strategy = if *sf >= 0.05 || *br >= 50 {
                    BuildStrategy::Split
                } else if *sf < 0.02 && *br < 20 {
                    BuildStrategy::Pch
                } else {
                    BuildStrategy::Passthrough
                };
                FileStrategyEntry {
                    path: path.clone(),
                    strategy: file_strategy,
                    src_frac: *sf,
                    fn_braces: *br,
                }
            })
            .collect();
        // Sort: split first (by braces desc), then passthrough, then pch
        entries.sort_by(|a, b| {
            let rank = |s: &BuildStrategy| match s {
                BuildStrategy::Split => 0,
                BuildStrategy::Passthrough => 1,
                BuildStrategy::Pch => 2,
            };
            rank(&a.strategy).cmp(&rank(&b.strategy))
                .then(b.fn_braces.cmp(&a.fn_braces))
        });
        entries
    } else {
        Vec::new()
    };

    StrategyResult {
        strategy,
        build_type,
        stale_frac,
        mean_src_frac: mean_src,
        mean_fn_braces: mean_brace,
        n_files: n,
        reason,
        per_file: per_file_entries,
    }
}

/// Only includes declarations that use safe builtin types (int, void, char, etc.)
/// to avoid dependencies on typedefs like size_t that may not be defined.
fn extract_extern_functions(filename: &str) -> FxHashMap<String, String> {
    use std::io::{BufRead, BufReader};

    let mut result: FxHashMap<String, String> = FxHashMap::default();

    let file = match File::open(filename) {
        Ok(f) => f,
        Err(_) => return result,
    };

    let reader = BufReader::new(file);

    // Use the pre-compiled static regex for extern function declarations
    // (avoids recompiling regex on every call - significant performance gain)

    // Types that are safe to use without typedefs
    let _safe_types = [
        "void", "int", "char", "short", "long", "float", "double",
        "unsigned", "signed", "const", "__fd_mask",
    ];

    // Types that indicate a dependency on typedefs - skip these declarations
    let unsafe_types = [
        "size_t", "ssize_t", "off_t", "off64_t", "pid_t", "uid_t", "gid_t",
        "mode_t", "dev_t", "ino_t", "nlink_t", "time_t", "clock_t",
        "socklen_t", "sa_family_t", "in_addr_t", "in_port_t",
        "FILE", "DIR", "fd_set", "sigset_t", "pthread_t",
        "va_list", "__gnuc_va_list", "__builtin_va_list",
        "intptr_t", "uintptr_t", "ptrdiff_t", "wchar_t", "wint_t", "wctype_t",
        "jmp_buf", "__jmp_buf_tag", "sigjmp_buf", "wctrans_t",
        "locale_t", "__locale_t", "nl_item",
        // glibc internal types (double-underscore prefixed)
        "__mode_t", "__uid_t", "__gid_t", "__pid_t", "__off_t", "__off64_t",
        "__dev_t", "__ino_t", "__ino64_t", "__nlink_t", "__time_t", "__clock_t",
        "__useconds_t", "__suseconds_t", "__blksize_t", "__blkcnt_t", "__blkcnt64_t",
        "__socklen_t", "__ssize_t", "__caddr_t", "__intptr_t", "__syscall_slong_t",
    ];

    // Builtin functions that should be skipped - these are handled by the compiler
    // NOTE: Most stdlib functions have been moved to stdlib_prototypes table for proper prototypes
    let builtin_functions = [
        // Character functions - return int used as boolean/arithmetic (compiler knows these)
        "isalpha", "isdigit", "isalnum", "isspace", "isupper", "islower",
        "toupper", "tolower", "isprint", "iscntrl", "ispunct", "isxdigit",
        // Wide character functions - use wint_t (compiler knows these)
        "iswupper", "iswlower", "iswdigit", "iswspace", "iswprint", "towupper", "towlower",
        // Locale/i18n - return char* (skip if already declared via stdlib_prototypes)
        "gettext", "ngettext", "dgettext", "dcgettext",
        // setjmp/longjmp - special handling (platform-specific)
        "setjmp", "longjmp", "sigsetjmp", "siglongjmp",
        // Variadic time functions - must use original prototypes from system headers
        "strftime", "wcsftime",
        // printf family - all variadic with format attributes
        "printf", "fprintf", "sprintf", "snprintf",
        "vprintf", "vfprintf", "vsprintf", "vsnprintf",
        "dprintf", "asprintf", "vasprintf",
        // scanf family - variadic
        "scanf", "fscanf", "sscanf", "vscanf", "vfscanf", "vsscanf",
        // exec family - some are variadic
        "execl", "execlp", "execle",
        // Other variadic system functions
        "syslog", "vsyslog", "err", "verr", "errx", "verrx", "warn", "vwarn", "warnx", "vwarnx",
        // GCC keywords that syntactically look like function calls but are NOT functions
        // Must never emit these as extern prototypes (causes "expected identifier" compile error)
        "typeof", "__typeof__", "__typeof", "_Generic",
        "__builtin_expect", "__builtin_unreachable", "__builtin_constant_p",
        "__builtin_offsetof", "__builtin_va_start", "__builtin_va_end",
        "__builtin_va_arg", "__builtin_va_copy", "__builtin_bswap16",
        "__builtin_bswap32", "__builtin_bswap64", "__builtin_clz",
        "__builtin_ctz", "__builtin_popcount", "__builtin_parity",
        "__builtin_ffs", "__builtin_abs", "__builtin_labs",
        "__builtin_alloca", "__builtin_memcpy", "__builtin_memset",
        "__builtin_strcmp", "__builtin_strlen", "__builtin_strcpy",
        "__builtin_prefetch", "__builtin_trap", "__builtin_return_address",
        "__builtin_frame_address", "__builtin_types_compatible_p",
        "__builtin_choose_expr", "__builtin_object_size",
        "__attribute__", "__asm__", "__asm", "asm",
        "_Static_assert", "__extension__",
    ];

    // Standard library function prototypes - use proper return types instead of void*
    // These are commonly used POSIX/libc functions that need correct prototypes
    let stdlib_prototypes: std::collections::HashMap<&str, &str> = [
        // Common string/memory functions (moved from builtin_functions for proper prototypes)
        ("strlen", "extern unsigned long strlen();"),
        ("strcpy", "extern char* strcpy();"),
        ("strncpy", "extern char* strncpy();"),
        ("strcat", "extern char* strcat();"),
        ("strncat", "extern char* strncat();"),
        ("strcmp", "extern int strcmp();"),
        ("strncmp", "extern int strncmp();"),
        ("strchr", "extern char* strchr();"),
        ("strrchr", "extern char* strrchr();"),
        ("strstr", "extern char* strstr();"),
        ("strdup", "extern char* strdup();"),
        ("strndup", "extern char* strndup();"),
        ("strerror", "extern char* strerror();"),
        ("strtok", "extern char* strtok();"),
        ("memcpy", "extern void* memcpy();"),
        ("memmove", "extern void* memmove();"),
        ("memset", "extern void* memset();"),
        ("memcmp", "extern int memcmp();"),
        ("memchr", "extern void* memchr();"),
        // Memory allocation
        ("malloc", "extern void* malloc();"),
        ("calloc", "extern void* calloc();"),
        ("realloc", "extern void* realloc();"),
        ("free", "extern void free();"),
        ("alloca", "extern void* alloca();"),
        // String functions returning size_t
        ("strcspn", "extern unsigned long strcspn();"),
        ("strspn", "extern unsigned long strspn();"),
        // Note: strftime omitted - compiler has special format attribute handling
        ("strxfrm", "extern unsigned long strxfrm();"),
        ("wcslen", "extern unsigned long wcslen();"),
        ("wcscspn", "extern unsigned long wcscspn();"),
        ("wcsspn", "extern unsigned long wcsspn();"),
        ("wcsxfrm", "extern unsigned long wcsxfrm();"),
        ("wcsftime", "extern unsigned long wcsftime();"),
        ("mbstowcs", "extern unsigned long mbstowcs();"),
        ("wcstombs", "extern unsigned long wcstombs();"),
        ("mbrlen", "extern unsigned long mbrlen();"),
        ("mbrtowc", "extern unsigned long mbrtowc();"),
        ("wcrtomb", "extern unsigned long wcrtomb();"),
        ("mbsrtowcs", "extern unsigned long mbsrtowcs();"),
        ("wcsrtombs", "extern unsigned long wcsrtombs();"),
        // String functions returning char*
        ("index", "extern char* index();"),
        ("rindex", "extern char* rindex();"),
        ("strsignal", "extern char* strsignal();"),
        ("strtok_r", "extern char* strtok_r();"),
        ("strsep", "extern char* strsep();"),
        ("stpcpy", "extern char* stpcpy();"),
        ("stpncpy", "extern char* stpncpy();"),
        ("basename", "extern char* basename();"),
        ("dirname", "extern char* dirname();"),
        ("realpath", "extern char* realpath();"),
        ("getcwd", "extern char* getcwd();"),
        ("getwd", "extern char* getwd();"),
        ("get_current_dir_name", "extern char* get_current_dir_name();"),
        ("getenv", "extern char* getenv();"),
        ("secure_getenv", "extern char* secure_getenv();"),
        ("getlogin", "extern char* getlogin();"),
        ("ttyname", "extern char* ttyname();"),
        ("ctermid", "extern char* ctermid();"),
        ("cuserid", "extern char* cuserid();"),
        ("tmpnam", "extern char* tmpnam();"),
        ("tempnam", "extern char* tempnam();"),
        ("mktemp", "extern char* mktemp();"),
        ("mkdtemp", "extern char* mkdtemp();"),
        ("setlocale", "extern char* setlocale();"),
        ("nl_langinfo", "extern char* nl_langinfo();"),
        ("ctime", "extern char* ctime();"),
        ("ctime_r", "extern char* ctime_r();"),
        ("asctime", "extern char* asctime();"),
        ("asctime_r", "extern char* asctime_r();"),
        ("bindtextdomain", "extern char* bindtextdomain();"),
        ("bind_textdomain_codeset", "extern char* bind_textdomain_codeset();"),
        ("textdomain", "extern char* textdomain();"),
        ("strerror_r", "extern char* strerror_r();"),
        ("inet_ntoa", "extern char* inet_ntoa();"),
        ("inet_ntop", "extern const char* inet_ntop();"),
        ("gai_strerror", "extern const char* gai_strerror();"),
        // Functions returning int
        ("close", "extern int close();"),
        ("creat", "extern int creat();"),
        ("read", "extern int read();"),
        ("write", "extern int write();"),
        ("pread", "extern int pread();"),
        ("pwrite", "extern int pwrite();"),
        ("dup", "extern int dup();"),
        ("dup2", "extern int dup2();"),
        ("dup3", "extern int dup3();"),
        ("pipe", "extern int pipe();"),
        ("pipe2", "extern int pipe2();"),
        // fcntl/ioctl/open are variadic but we use K&R style for function pointer usage
        // (e.g., SQLite's syscall table casts them to void(*)(void))
        ("fcntl", "extern int fcntl();"),
        ("ioctl", "extern int ioctl();"),
        ("open", "extern int open();"),
        ("openat", "extern int openat();"),
        ("flock", "extern int flock();"),
        ("fsync", "extern int fsync();"),
        ("fdatasync", "extern int fdatasync();"),
        ("ftruncate", "extern int ftruncate();"),
        ("truncate", "extern int truncate();"),
        ("access", "extern int access();"),
        ("faccessat", "extern int faccessat();"),
        ("chown", "extern int chown();"),
        ("fchown", "extern int fchown();"),
        ("lchown", "extern int lchown();"),
        ("fchownat", "extern int fchownat();"),
        ("chmod", "extern int chmod();"),
        ("fchmod", "extern int fchmod();"),
        ("fchmodat", "extern int fchmodat();"),
        ("stat", "extern int stat();"),
        ("fstat", "extern int fstat();"),
        ("lstat", "extern int lstat();"),
        ("fstatat", "extern int fstatat();"),
        ("mkdir", "extern int mkdir();"),
        ("mkdirat", "extern int mkdirat();"),
        ("rmdir", "extern int rmdir();"),
        ("link", "extern int link();"),
        ("linkat", "extern int linkat();"),
        ("unlink", "extern int unlink();"),
        ("unlinkat", "extern int unlinkat();"),
        ("symlink", "extern int symlink();"),
        ("symlinkat", "extern int symlinkat();"),
        ("readlink", "extern int readlink();"),
        ("readlinkat", "extern int readlinkat();"),
        ("rename", "extern int rename();"),
        ("renameat", "extern int renameat();"),
        ("chdir", "extern int chdir();"),
        ("fchdir", "extern int fchdir();"),
        ("utimes", "extern int utimes();"),
        ("futimes", "extern int futimes();"),
        ("lutimes", "extern int lutimes();"),
        ("utimensat", "extern int utimensat();"),
        ("futimens", "extern int futimens();"),
        ("utime", "extern int utime();"),
        ("mknod", "extern int mknod();"),
        ("mknodat", "extern int mknodat();"),
        ("mkfifo", "extern int mkfifo();"),
        ("mkfifoat", "extern int mkfifoat();"),
        ("umask", "extern int umask();"),
        ("socket", "extern int socket();"),
        ("socketpair", "extern int socketpair();"),
        ("bind", "extern int bind();"),
        ("listen", "extern int listen();"),
        ("accept", "extern int accept();"),
        ("accept4", "extern int accept4();"),
        ("connect", "extern int connect();"),
        ("shutdown", "extern int shutdown();"),
        ("send", "extern int send();"),
        ("sendto", "extern int sendto();"),
        ("recv", "extern int recv();"),
        ("recvfrom", "extern int recvfrom();"),
        ("sendmsg", "extern int sendmsg();"),
        ("recvmsg", "extern int recvmsg();"),
        ("getsockopt", "extern int getsockopt();"),
        ("setsockopt", "extern int setsockopt();"),
        ("getsockname", "extern int getsockname();"),
        ("getpeername", "extern int getpeername();"),
        ("getaddrinfo", "extern int getaddrinfo();"),
        ("getnameinfo", "extern int getnameinfo();"),
        ("inet_aton", "extern int inet_aton();"),
        ("inet_pton", "extern int inet_pton();"),
        ("poll", "extern int poll();"),
        ("ppoll", "extern int ppoll();"),
        ("select", "extern int select();"),
        ("pselect", "extern int pselect();"),
        ("epoll_create", "extern int epoll_create();"),
        ("epoll_create1", "extern int epoll_create1"),
        ("epoll_ctl", "extern int epoll_ctl();"),
        ("epoll_wait", "extern int epoll_wait();"),
        ("epoll_pwait", "extern int epoll_pwait();"),
        ("eventfd", "extern int eventfd();"),
        ("signalfd", "extern int signalfd();"),
        ("timerfd_create", "extern int timerfd_create();"),
        ("timerfd_settime", "extern int timerfd_settime();"),
        ("timerfd_gettime", "extern int timerfd_gettime();"),
        ("inotify_init", "extern int inotify_init();"),
        ("inotify_init1", "extern int inotify_init1();"),
        ("inotify_add_watch", "extern int inotify_add_watch();"),
        ("inotify_rm_watch", "extern int inotify_rm_watch();"),
        ("fork", "extern int fork();"),
        ("vfork", "extern int vfork();"),
        ("execve", "extern int execve();"),
        ("execv", "extern int execv();"),
        ("execvp", "extern int execvp();"),
        ("execvpe", "extern int execvpe();"),
        ("execl", "extern int execl();"),
        ("execlp", "extern int execlp();"),
        ("execle", "extern int execle();"),
        ("fexecve", "extern int fexecve();"),
        ("system", "extern int system();"),
        ("wait", "extern int wait();"),
        ("waitpid", "extern int waitpid();"),
        ("waitid", "extern int waitid();"),
        ("wait3", "extern int wait3();"),
        ("wait4", "extern int wait4();"),
        ("kill", "extern int kill();"),
        ("killpg", "extern int killpg();"),
        ("raise", "extern int raise();"),
        ("sigaction", "extern int sigaction();"),
        ("sigprocmask", "extern int sigprocmask();"),
        ("sigpending", "extern int sigpending();"),
        ("sigsuspend", "extern int sigsuspend();"),
        ("sigwait", "extern int sigwait();"),
        ("sigwaitinfo", "extern int sigwaitinfo();"),
        ("sigtimedwait", "extern int sigtimedwait();"),
        ("sigqueue", "extern int sigqueue();"),
        ("sigemptyset", "extern int sigemptyset();"),
        ("sigfillset", "extern int sigfillset();"),
        ("sigaddset", "extern int sigaddset();"),
        ("sigdelset", "extern int sigdelset();"),
        ("sigismember", "extern int sigismember();"),
        ("alarm", "extern unsigned int alarm();"),
        ("sleep", "extern unsigned int sleep();"),
        ("usleep", "extern int usleep();"),
        ("nanosleep", "extern int nanosleep();"),
        ("clock_nanosleep", "extern int clock_nanosleep();"),
        ("getpid", "extern int getpid();"),
        ("getppid", "extern int getppid();"),
        ("getpgid", "extern int getpgid();"),
        ("getpgrp", "extern int getpgrp();"),
        ("setpgid", "extern int setpgid();"),
        ("setpgrp", "extern int setpgrp();"),
        ("getsid", "extern int getsid();"),
        ("setsid", "extern int setsid();"),
        ("getuid", "extern int getuid();"),
        ("geteuid", "extern int geteuid();"),
        ("getgid", "extern int getgid();"),
        ("getegid", "extern int getegid();"),
        ("setuid", "extern int setuid();"),
        ("seteuid", "extern int seteuid();"),
        ("setgid", "extern int setgid();"),
        ("setegid", "extern int setegid();"),
        ("setreuid", "extern int setreuid();"),
        ("setregid", "extern int setregid();"),
        ("setresuid", "extern int setresuid();"),
        ("setresgid", "extern int setresgid();"),
        ("getresuid", "extern int getresuid();"),
        ("getresgid", "extern int getresgid();"),
        ("getgroups", "extern int getgroups();"),
        ("setgroups", "extern int setgroups();"),
        ("initgroups", "extern int initgroups();"),
        ("nice", "extern int nice();"),
        ("getpriority", "extern int getpriority();"),
        ("setpriority", "extern int setpriority();"),
        ("getrlimit", "extern int getrlimit();"),
        ("setrlimit", "extern int setrlimit();"),
        ("prlimit", "extern int prlimit();"),
        ("getrusage", "extern int getrusage();"),
        ("times", "extern int times();"),
        ("time", "extern int time();"),
        ("clock_gettime", "extern int clock_gettime();"),
        ("clock_settime", "extern int clock_settime();"),
        ("clock_getres", "extern int clock_getres();"),
        ("gettimeofday", "extern int gettimeofday();"),
        ("settimeofday", "extern int settimeofday();"),
        ("adjtime", "extern int adjtime();"),
        ("localtime", "extern void* localtime();"),
        ("localtime_r", "extern void* localtime_r();"),
        ("gmtime", "extern void* gmtime();"),
        ("gmtime_r", "extern void* gmtime_r();"),
        ("mktime", "extern int mktime();"),
        ("timegm", "extern int timegm();"),
        ("difftime", "extern double difftime();"),
        ("mmap", "extern void* mmap();"),
        ("mmap64", "extern void* mmap64();"),
        ("munmap", "extern int munmap();"),
        ("mremap", "extern void* mremap();"),
        ("mprotect", "extern int mprotect();"),
        ("msync", "extern int msync();"),
        ("mlock", "extern int mlock();"),
        ("munlock", "extern int munlock();"),
        ("mlockall", "extern int mlockall();"),
        ("munlockall", "extern int munlockall();"),
        ("madvise", "extern int madvise();"),
        ("mincore", "extern int mincore();"),
        ("shm_open", "extern int shm_open();"),
        ("shm_unlink", "extern int shm_unlink();"),
        ("shmget", "extern int shmget();"),
        ("shmat", "extern void* shmat();"),
        ("shmdt", "extern int shmdt();"),
        ("shmctl", "extern int shmctl();"),
        ("semget", "extern int semget();"),
        ("semop", "extern int semop();"),
        ("semctl", "extern int semctl();"),
        ("msgget", "extern int msgget();"),
        ("msgsnd", "extern int msgsnd();"),
        ("msgrcv", "extern int msgrcv();"),
        ("msgctl", "extern int msgctl();"),
        ("dlopen", "extern void* dlopen();"),
        ("dlclose", "extern int dlclose();"),
        ("dlsym", "extern void* dlsym();"),
        ("dlerror", "extern char* dlerror();"),
        ("dladdr", "extern int dladdr();"),
        ("pthread_create", "extern int pthread_create();"),
        ("pthread_join", "extern int pthread_join();"),
        ("pthread_detach", "extern int pthread_detach();"),
        ("pthread_exit", "extern void pthread_exit();"),
        ("pthread_self", "extern unsigned long pthread_self();"),
        ("pthread_equal", "extern int pthread_equal();"),
        ("pthread_cancel", "extern int pthread_cancel();"),
        ("pthread_setcancelstate", "extern int pthread_setcancelstate();"),
        ("pthread_setcanceltype", "extern int pthread_setcanceltype();"),
        ("pthread_testcancel", "extern void pthread_testcancel();"),
        ("pthread_mutex_init", "extern int pthread_mutex_init();"),
        ("pthread_mutex_destroy", "extern int pthread_mutex_destroy();"),
        ("pthread_mutex_lock", "extern int pthread_mutex_lock();"),
        ("pthread_mutex_trylock", "extern int pthread_mutex_trylock();"),
        ("pthread_mutex_unlock", "extern int pthread_mutex_unlock();"),
        ("pthread_cond_init", "extern int pthread_cond_init();"),
        ("pthread_cond_destroy", "extern int pthread_cond_destroy();"),
        ("pthread_cond_signal", "extern int pthread_cond_signal();"),
        ("pthread_cond_broadcast", "extern int pthread_cond_broadcast();"),
        ("pthread_cond_wait", "extern int pthread_cond_wait();"),
        ("pthread_cond_timedwait", "extern int pthread_cond_timedwait();"),
        ("pthread_rwlock_init", "extern int pthread_rwlock_init();"),
        ("pthread_rwlock_destroy", "extern int pthread_rwlock_destroy();"),
        ("pthread_rwlock_rdlock", "extern int pthread_rwlock_rdlock();"),
        ("pthread_rwlock_wrlock", "extern int pthread_rwlock_wrlock();"),
        ("pthread_rwlock_tryrdlock", "extern int pthread_rwlock_tryrdlock();"),
        ("pthread_rwlock_trywrlock", "extern int pthread_rwlock_trywrlock();"),
        ("pthread_rwlock_unlock", "extern int pthread_rwlock_unlock();"),
        ("pthread_key_create", "extern int pthread_key_create();"),
        ("pthread_key_delete", "extern int pthread_key_delete();"),
        ("pthread_getspecific", "extern void* pthread_getspecific();"),
        ("pthread_setspecific", "extern int pthread_setspecific();"),
        ("pthread_once", "extern int pthread_once();"),
        ("pthread_atfork", "extern int pthread_atfork();"),
        ("pthread_attr_init", "extern int pthread_attr_init();"),
        ("pthread_attr_destroy", "extern int pthread_attr_destroy();"),
        ("pthread_attr_setdetachstate", "extern int pthread_attr_setdetachstate();"),
        ("pthread_attr_getdetachstate", "extern int pthread_attr_getdetachstate();"),
        ("pthread_attr_setstacksize", "extern int pthread_attr_setstacksize();"),
        ("pthread_attr_getstacksize", "extern int pthread_attr_getstacksize();"),
        ("sysconf", "extern long sysconf();"),
        ("pathconf", "extern long pathconf();"),
        ("fpathconf", "extern long fpathconf();"),
        ("confstr", "extern unsigned long confstr();"),
        ("getopt", "extern int getopt();"),
        ("getopt_long", "extern int getopt_long();"),
        ("getopt_long_only", "extern int getopt_long_only();"),
        ("opendir", "extern void* opendir();"),
        ("fdopendir", "extern void* fdopendir();"),
        ("closedir", "extern int closedir();"),
        ("readdir", "extern void* readdir();"),
        ("readdir_r", "extern int readdir_r();"),
        ("rewinddir", "extern void rewinddir();"),
        ("seekdir", "extern void seekdir();"),
        ("telldir", "extern long telldir();"),
        ("dirfd", "extern int dirfd();"),
        ("scandir", "extern int scandir();"),
        ("alphasort", "extern int alphasort();"),
        ("glob", "extern int glob();"),
        ("globfree", "extern void globfree();"),
        ("fnmatch", "extern int fnmatch();"),
        ("fopen", "extern void* fopen();"),
        ("fopen64", "extern void* fopen64();"),
        ("fdopen", "extern void* fdopen();"),
        ("freopen", "extern void* freopen();"),
        ("fclose", "extern int fclose();"),
        ("fflush", "extern int fflush();"),
        ("fread", "extern unsigned long fread();"),
        ("fwrite", "extern unsigned long fwrite();"),
        ("fgetc", "extern int fgetc();"),
        ("getc", "extern int getc();"),
        ("getchar", "extern int getchar();"),
        ("fgets", "extern char* fgets();"),
        ("gets", "extern char* gets();"),
        ("fputc", "extern int fputc();"),
        ("putc", "extern int putc();"),
        ("putchar", "extern int putchar();"),
        ("fputs", "extern int fputs();"),
        ("puts", "extern int puts();"),
        ("ungetc", "extern int ungetc();"),
        ("fseek", "extern int fseek();"),
        ("fseeko", "extern int fseeko();"),
        ("ftell", "extern long ftell();"),
        ("ftello", "extern long ftello();"),
        ("rewind", "extern void rewind();"),
        ("fgetpos", "extern int fgetpos();"),
        ("fsetpos", "extern int fsetpos();"),
        ("clearerr", "extern void clearerr();"),
        ("feof", "extern int feof();"),
        ("ferror", "extern int ferror();"),
        ("fileno", "extern int fileno();"),
        ("setvbuf", "extern int setvbuf();"),
        ("setbuf", "extern void setbuf();"),
        ("setbuffer", "extern void setbuffer();"),
        ("setlinebuf", "extern void setlinebuf();"),
        ("fprintf", "extern int fprintf();"),
        ("printf", "extern int printf();"),
        ("sprintf", "extern int sprintf();"),
        ("snprintf", "extern int snprintf();"),
        ("vfprintf", "extern int vfprintf();"),
        ("vprintf", "extern int vprintf();"),
        ("vsprintf", "extern int vsprintf();"),
        ("vsnprintf", "extern int vsnprintf();"),
        ("fscanf", "extern int fscanf();"),
        ("scanf", "extern int scanf();"),
        ("sscanf", "extern int sscanf();"),
        ("vfscanf", "extern int vfscanf();"),
        ("vscanf", "extern int vscanf();"),
        ("vsscanf", "extern int vsscanf();"),
        ("popen", "extern void* popen();"),
        ("pclose", "extern int pclose();"),
        ("perror", "extern void perror();"),
        ("remove", "extern int remove();"),
        ("mkstemp", "extern int mkstemp();"),
        ("mkstemps", "extern int mkstemps();"),
        ("mkostemps", "extern int mkostemps();"),
        ("getline", "extern int getline();"),
        ("getdelim", "extern int getdelim();"),
        ("fmemopen", "extern void* fmemopen();"),
        ("open_memstream", "extern void* open_memstream();"),
        ("atoi", "extern int atoi();"),
        ("atol", "extern long atol();"),
        ("atoll", "extern long long atoll();"),
        ("atof", "extern double atof();"),
        ("strtol", "extern long strtol();"),
        ("strtoll", "extern long long strtoll();"),
        ("strtoul", "extern unsigned long strtoul();"),
        ("strtoull", "extern unsigned long long strtoull();"),
        ("strtof", "extern float strtof();"),
        ("strtod", "extern double strtod();"),
        ("strtold", "extern long double strtold();"),
        ("abs", "extern int abs();"),
        ("labs", "extern long labs();"),
        ("llabs", "extern long long llabs();"),
        ("div", "extern int div();"),
        ("ldiv", "extern long ldiv();"),
        ("lldiv", "extern long long lldiv();"),
        ("rand", "extern int rand();"),
        ("rand_r", "extern int rand_r();"),
        ("srand", "extern void srand();"),
        ("random", "extern long random();"),
        ("srandom", "extern void srandom();"),
        ("initstate", "extern char* initstate();"),
        ("setstate", "extern char* setstate();"),
        ("drand48", "extern double drand48();"),
        ("erand48", "extern double erand48();"),
        ("lrand48", "extern long lrand48();"),
        ("nrand48", "extern long nrand48();"),
        ("mrand48", "extern long mrand48();"),
        ("jrand48", "extern long jrand48();"),
        ("srand48", "extern void srand48();"),
        ("seed48", "extern unsigned short* seed48();"),
        ("lcong48", "extern void lcong48();"),
        ("bsearch", "extern void* bsearch();"),
        ("qsort", "extern void qsort();"),
        ("qsort_r", "extern void qsort_r();"),
        ("atexit", "extern int atexit();"),
        ("on_exit", "extern int on_exit();"),
        ("exit", "extern void exit();"),
        ("_exit", "extern void _exit();"),
        ("_Exit", "extern void _Exit();"),
        ("abort", "extern void abort();"),
        ("quick_exit", "extern void quick_exit();"),
        ("at_quick_exit", "extern int at_quick_exit();"),
        ("getloadavg", "extern int getloadavg();"),
        ("assert", "extern void assert();"),
        ("isatty", "extern int isatty();"),
        ("ttyname_r", "extern int ttyname_r();"),
        ("tcgetattr", "extern int tcgetattr();"),
        ("tcsetattr", "extern int tcsetattr();"),
        ("tcsendbreak", "extern int tcsendbreak();"),
        ("tcdrain", "extern int tcdrain();"),
        ("tcflush", "extern int tcflush();"),
        ("tcflow", "extern int tcflow();"),
        ("tcgetpgrp", "extern int tcgetpgrp();"),
        ("tcsetpgrp", "extern int tcsetpgrp();"),
        ("cfgetispeed", "extern unsigned int cfgetispeed();"),
        ("cfgetospeed", "extern unsigned int cfgetospeed();"),
        ("cfsetispeed", "extern int cfsetispeed();"),
        ("cfsetospeed", "extern int cfsetospeed();"),
        ("cfsetspeed", "extern int cfsetspeed();"),
        ("cfmakeraw", "extern void cfmakeraw();"),
        ("openpty", "extern int openpty();"),
        ("forkpty", "extern int forkpty();"),
        ("login_tty", "extern int login_tty();"),
        ("ptsname", "extern char* ptsname();"),
        ("ptsname_r", "extern int ptsname_r();"),
        ("grantpt", "extern int grantpt();"),
        ("unlockpt", "extern int unlockpt();"),
        ("posix_openpt", "extern int posix_openpt();"),
        ("getpass", "extern char* getpass();"),
        ("crypt", "extern char* crypt();"),
        ("crypt_r", "extern char* crypt_r();"),
        ("setkey", "extern void setkey();"),
        ("encrypt", "extern void encrypt();"),
        ("ecvt", "extern char* ecvt();"),
        ("fcvt", "extern char* fcvt();"),
        ("gcvt", "extern char* gcvt();"),
        ("lseek", "extern long lseek();"),
        ("lseek64", "extern long long lseek64();"),
        ("sbrk", "extern void* sbrk();"),
        ("brk", "extern int brk();"),
        ("getpagesize", "extern int getpagesize();"),
        ("gethostname", "extern int gethostname();"),
        ("sethostname", "extern int sethostname();"),
        ("getdomainname", "extern int getdomainname();"),
        ("setdomainname", "extern int setdomainname();"),
        ("uname", "extern int uname();"),
        ("gethostid", "extern long gethostid();"),
        ("sethostid", "extern int sethostid();"),
        ("sync", "extern void sync();"),
        ("syncfs", "extern int syncfs();"),
        ("statfs", "extern int statfs();"),
        ("fstatfs", "extern int fstatfs();"),
        ("statvfs", "extern int statvfs();"),
        ("fstatvfs", "extern int fstatvfs();"),
        ("getmntent", "extern void* getmntent();"),
        ("setmntent", "extern void* setmntent();"),
        ("addmntent", "extern int addmntent();"),
        ("endmntent", "extern int endmntent();"),
        ("hasmntopt", "extern char* hasmntopt();"),
        ("setpwent", "extern void setpwent();"),
        ("endpwent", "extern void endpwent();"),
        ("getpwent", "extern void* getpwent();"),
        ("getpwuid", "extern void* getpwuid();"),
        ("getpwnam", "extern void* getpwnam();"),
        ("getpwuid_r", "extern int getpwuid_r();"),
        ("getpwnam_r", "extern int getpwnam_r();"),
        ("setgrent", "extern void setgrent();"),
        ("endgrent", "extern void endgrent();"),
        ("getgrent", "extern void* getgrent();"),
        ("getgrgid", "extern void* getgrgid();"),
        ("getgrnam", "extern void* getgrnam();"),
        ("getgrgid_r", "extern int getgrgid_r();"),
        ("getgrnam_r", "extern int getgrnam_r();"),
        ("getspent", "extern void* getspent();"),
        ("getspnam", "extern void* getspnam();"),
        ("setspent", "extern void setspent();"),
        ("endspent", "extern void endspent();"),
        ("fgetspent", "extern void* fgetspent();"),
        ("putspent", "extern int putspent();"),
        ("getspnam_r", "extern int getspnam_r();"),
        ("lckpwdf", "extern int lckpwdf();"),
        ("ulckpwdf", "extern int ulckpwdf();"),
        ("getlogin_r", "extern int getlogin_r();"),
        ("setutent", "extern void setutent();"),
        ("endutent", "extern void endutent();"),
        ("getutent", "extern void* getutent();"),
        ("getutid", "extern void* getutid();"),
        ("getutline", "extern void* getutline();"),
        ("pututline", "extern void* pututline();"),
        ("utmpname", "extern int utmpname();"),
        ("setutxent", "extern void setutxent();"),
        ("endutxent", "extern void endutxent();"),
        ("getutxent", "extern void* getutxent();"),
        ("getutxid", "extern void* getutxid();"),
        ("getutxline", "extern void* getutxline();"),
        ("pututxline", "extern void* pututxline();"),
        ("updwtmp", "extern void updwtmp();"),
        ("updwtmpx", "extern void updwtmpx();"),
        ("logwtmp", "extern void logwtmp();"),
        ("syslog", "extern void syslog();"),
        ("vsyslog", "extern void vsyslog();"),
        ("openlog", "extern void openlog();"),
        ("closelog", "extern void closelog();"),
        ("setlogmask", "extern int setlogmask();"),
        ("err", "extern void err();"),
        ("verr", "extern void verr();"),
        ("errx", "extern void errx();"),
        ("verrx", "extern void verrx();"),
        ("warn", "extern void warn();"),
        ("vwarn", "extern void vwarn();"),
        ("warnx", "extern void warnx();"),
        ("vwarnx", "extern void vwarnx();"),
        ("error", "extern void error();"),
        ("error_at_line", "extern void error_at_line();"),
        ("posix_memalign", "extern int posix_memalign();"),
        ("aligned_alloc", "extern void* aligned_alloc();"),
        ("valloc", "extern void* valloc();"),
        ("pvalloc", "extern void* pvalloc();"),
        ("memalign", "extern void* memalign();"),
        ("reallocarray", "extern void* reallocarray();"),
        ("getentropy", "extern int getentropy();"),
        ("getrandom", "extern int getrandom();"),
        ("arc4random", "extern unsigned int arc4random();"),
        ("arc4random_uniform", "extern unsigned int arc4random_uniform();"),
        ("arc4random_buf", "extern void arc4random_buf();"),
        ("regcomp", "extern int regcomp();"),
        ("regexec", "extern int regexec();"),
        ("regerror", "extern unsigned long regerror();"),
        ("regfree", "extern void regfree();"),
        ("iconv_open", "extern void* iconv_open();"),
        ("iconv_close", "extern int iconv_close();"),
        ("iconv", "extern unsigned long iconv();"),
        // Bug62: glibc ctype.h internal functions - return pointer to pointer
        // Used in isdigit/isalpha etc. macros after preprocessing
        ("__ctype_b_loc", "extern const unsigned short int **__ctype_b_loc();"),
        ("__ctype_tolower_loc", "extern const int **__ctype_tolower_loc();"),
        ("__ctype_toupper_loc", "extern const int **__ctype_toupper_loc();"),
    ].into_iter().collect();

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };

        let trimmed = line.trim();

        // Skip empty lines, preprocessor directives
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        // Check if this is an extern function declaration
        if !trimmed.starts_with("extern ") {
            continue;
        }

        // Must be a function declaration (has opening parenthesis)
        if !trimmed.contains('(') {
            continue;
        }

        // Check if this is a complete single-line declaration
        let is_complete = trimmed.ends_with(';') && trimmed.contains(')');

        // Check if declaration uses unsafe types (require typedefs)
        // OPTIMIZATION: Instead of compiling a regex for each unsafe type (O(N*M) regex compiles),
        // tokenize the line once and check set membership (O(N) tokenize + O(M) lookups)
        let line_identifiers: FxHashSet<&str> = tokenize_c_identifiers(trimmed).collect();
        let uses_unsafe_type = unsafe_types.iter().any(|t| line_identifiers.contains(*t));

        // Extract function name using pre-compiled static regex
        if let Some(caps) = EXTERN_FUNC_RE.captures(trimmed) {
            if let Some(name_match) = caps.get(1) {
                let func_name = name_match.as_str().to_string();

                // Check if void* stub generation is enabled (for sqlite compatibility)
                // Default is off (0) for vim compatibility, set VOID_STUB=1 for sqlite
                let void_stub_enabled = std::env::var("VOID_STUB")
                    .map(|v| v == "1")
                    .unwrap_or(false);

                // Store the declaration (without duplicate entries)
                if !result.contains_key(&func_name) {
                    // First check if this is a builtin/variadic function that should be skipped
                    // These functions have special compiler handling or are variadic, so we
                    // must not emit simplified prototypes that would conflict
                    if builtin_functions.contains(&func_name.as_str()) {
                        // Skip - handled by the compiler or has original declaration in source
                        continue;
                    } else if let Some(proto) = stdlib_prototypes.get(func_name.as_str()) {
                        // Use the known correct prototype from our table
                        if std::env::var("DEBUG_BUG66").is_ok() {
                            eprintln!("DEBUG Bug66: Adding {} from stdlib_prototypes: {}", func_name, proto);
                        }
                        result.insert(func_name, proto.to_string());
                    } else if is_complete && !uses_unsafe_type {
                        // Complete single-line declaration without unsafe types - use original
                        result.insert(func_name, trimmed.to_string());
                    } else if void_stub_enabled {
                        // Fallback to void* stub for unknown functions with unsafe types
                        let simplified = format!("extern void* {}();", func_name);
                        result.insert(func_name, simplified);
                    }
                    // When void_stub_enabled is false, skip declarations with unsafe types
                    // as they would conflict with compiler builtins or require typedefs
                }
            }
        }
    }

    result
}

/// Bug48 + Bug17: Extract extern variable declarations from a preprocessed file.
/// These are extern variable declarations that ctags doesn't capture as variables.
/// Patterns:
///   - extern const struct <type> <variable_name>;  (Bug48)
///   - extern <type> <variable_name>[];             (Bug17 - arrays)
///   - extern <type> <variable_name>;               (Bug17 - simple vars)
/// Returns a HashMap of variable_name -> full_declaration.
fn extract_extern_variables(filename: &str) -> FxHashMap<String, String> {
    use std::io::{BufRead, BufReader};
    use once_cell::sync::Lazy;

    let mut result: FxHashMap<String, String> = FxHashMap::default();

    let file = match File::open(filename) {
        Ok(f) => f,
        Err(_) => return result,
    };

    let reader = BufReader::new(file);

    // Pattern 1: extern const struct <type> *?<variable_name>;
    static EXTERN_CONST_STRUCT_RE: Lazy<regex::Regex> = Lazy::new(|| {
        regex::Regex::new(r"^extern\s+const\s+struct\s+(\w+)\s+\*?\s*(\w+)\s*;").unwrap()
    });

    // Pattern 2: extern <type> <variable_name>[]; (array declarations)
    // Matches: extern char name[]; extern int arr[]; extern char *name[];
    static EXTERN_ARRAY_RE: Lazy<regex::Regex> = Lazy::new(|| {
        regex::Regex::new(r"^extern\s+\w+\s+\*?\s*(\w+)\s*\[\s*\]\s*;").unwrap()
    });

    // Pattern 3: extern <type> <variable_name>; (simple variable declarations)
    // But NOT function declarations (those have parentheses)
    // Matches: extern int count; extern char *ptr; extern volatile sig_atomic_t got_int;
    // Also matches multi-word type qualifiers: volatile, restrict, __volatile__
    static EXTERN_SIMPLE_RE: Lazy<regex::Regex> = Lazy::new(|| {
        regex::Regex::new(r"^extern\s+(?:const\s+)?(?:volatile\s+)?(?:unsigned\s+|signed\s+)?(?:long\s+|short\s+)?(?:struct\s+\w+\s+|union\s+\w+\s+|enum\s+\w+\s+)?(?:\w+\s+)+\*?\s*(\w+)\s*;").unwrap()
    });

    // Pattern 4: extern <type> (*<variable_name>)(...); (function pointer variable declarations)
    // Matches: extern int (*mb_ptr2len)(char_u *p);
    static EXTERN_FUNCPTR_RE: Lazy<regex::Regex> = Lazy::new(|| {
        regex::Regex::new(r"^extern\s+(?:\w+\s+)+\(\s*\*\s*(\w+)\s*\)\s*\(").unwrap()
    });

    // Pattern 5: extern <type> <variable_name>[] (array decl with semicolon on next line)
    // Matches line without trailing semicolon: extern char name[]
    static EXTERN_ARRAY_NOSEMI_RE: Lazy<regex::Regex> = Lazy::new(|| {
        regex::Regex::new(r"^extern\s+\w+\s+\*?\s*(\w+)\s*\[\s*\]?\s*$").unwrap()
    });

    let mut pending_extern: Option<(String, String)> = None; // (var_name, decl_line)

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };

        let trimmed = line.trim();

        // Handle continuation: previous line was an extern decl without semicolon
        if let Some((var_name, decl_line)) = pending_extern.take() {
            // Skip blank lines between the declaration and the semicolon
            if trimmed.is_empty() {
                pending_extern = Some((var_name, decl_line));
                continue;
            }
            // Current line should be just ";" (semicolon continuation)
            if trimmed == ";" || trimmed.starts_with(';') {
                // Complete declaration: combine the lines
                let full_decl = format!("{};", decl_line);
                if !result.contains_key(&var_name) {
                    result.insert(var_name, full_decl);
                }
            }
            // Either way, don't fall through to process as new extern
            continue;
        }

        // Skip preprocessor directives
        if trimmed.starts_with('#') {
            continue;
        }

        // Pattern 4: extern function pointer variable (must check before skipping '(' lines)
        if trimmed.contains("(*") {
            if let Some(caps) = EXTERN_FUNCPTR_RE.captures(trimmed) {
                if let Some(name_match) = caps.get(1) {
                    let var_name = name_match.as_str().to_string();
                    if !result.contains_key(&var_name) {
                        result.insert(var_name, trimmed.to_string());
                    }
                }
            }
            continue;
        }

        // Skip function declarations (have parentheses)
        if trimmed.contains('(') {
            continue;
        }

        // Try pattern 1: extern const struct
        if let Some(caps) = EXTERN_CONST_STRUCT_RE.captures(trimmed) {
            if let Some(name_match) = caps.get(2) {
                let var_name = name_match.as_str().to_string();
                if !result.contains_key(&var_name) {
                    result.insert(var_name, trimmed.to_string());
                }
                continue;
            }
        }

        // Try pattern 2: extern array (with semicolon on same line)
        if let Some(caps) = EXTERN_ARRAY_RE.captures(trimmed) {
            if let Some(name_match) = caps.get(1) {
                let var_name = name_match.as_str().to_string();
                if !result.contains_key(&var_name) {
                    result.insert(var_name, trimmed.to_string());
                }
                continue;
            }
        }

        // Try pattern 5: extern array without semicolon (semicolon on next line)
        if trimmed.starts_with("extern") && !trimmed.ends_with(';') {
            if let Some(caps) = EXTERN_ARRAY_NOSEMI_RE.captures(trimmed) {
                if let Some(name_match) = caps.get(1) {
                    let var_name = name_match.as_str().to_string();
                    pending_extern = Some((var_name, trimmed.to_string()));
                    continue;
                }
            }
        }

        // Try pattern 3: extern simple variable
        if let Some(caps) = EXTERN_SIMPLE_RE.captures(trimmed) {
            if let Some(name_match) = caps.get(1) {
                let var_name = name_match.as_str().to_string();
                if !result.contains_key(&var_name) {
                    result.insert(var_name, trimmed.to_string());
                }
            }
        }
    }

    result
}

/// Bug78 fix: Extract the type name from an extern variable declaration.
/// Returns Some(type_name) if a custom type is found, None for basic C types.
/// Examples:
///   "extern clipmethod_T clipmethod;" -> Some("clipmethod_T")
///   "extern char **environ;" -> None (basic type)
///   "extern int count;" -> None (basic type)
///   "extern FILE *fp;" -> Some("FILE")
fn extract_type_from_extern_var_decl(decl: &str) -> Option<&str> {
    // Basic C types that don't need typedef definitions
    const BASIC_TYPES: &[&str] = &[
        "void", "char", "short", "int", "long", "float", "double",
        "signed", "unsigned", "const", "volatile", "restrict",
        "size_t", "ssize_t", "ptrdiff_t", "intptr_t", "uintptr_t",
        "int8_t", "int16_t", "int32_t", "int64_t",
        "uint8_t", "uint16_t", "uint32_t", "uint64_t",
        "off_t", "time_t", "pid_t", "uid_t", "gid_t", "mode_t",
        "dev_t", "ino_t", "nlink_t", "blksize_t", "blkcnt_t",
        "socklen_t", "sa_family_t", "in_addr_t", "in_port_t",
        "pthread_t", "pthread_mutex_t", "pthread_cond_t",
        "va_list", "jmp_buf", "sigjmp_buf",
        "_Bool", "bool", "wchar_t", "wint_t",
    ];

    let trimmed = decl.trim();

    // Skip if not an extern declaration
    if !trimmed.starts_with("extern") {
        return None;
    }

    // Remove "extern " prefix and optional qualifiers
    let rest = trimmed.strip_prefix("extern")?.trim();

    // Skip "const", "volatile", "struct", "union", "enum" prefixes
    let mut words: Vec<&str> = rest.split_whitespace().collect();
    if words.is_empty() {
        return None;
    }

    // Skip qualifiers
    while !words.is_empty() && ["const", "volatile", "register"].contains(&words[0]) {
        words.remove(0);
    }

    if words.is_empty() {
        return None;
    }

    // Handle "struct X", "union X", "enum X"
    if ["struct", "union", "enum"].contains(&words[0]) {
        // struct/union/enum are fine - they're defined separately
        return None;
    }

    // Get the type name (first word after qualifiers, before pointers/variable name)
    let type_name = words[0].trim_end_matches('*');

    // Check if it's a basic type
    if BASIC_TYPES.contains(&type_name) {
        return None;
    }

    // Check for compound basic types (unsigned long, signed int, etc.)
    if ["unsigned", "signed", "long", "short"].contains(&type_name) {
        return None;
    }

    // Return the custom type name
    Some(type_name)
}

/// Bug71 fix: Extract static function pointer variable declarations that ctags doesn't capture.
/// Pattern: static <type> *((*name)(<params>));
/// Example: static char_u *((*set_opt_callback_func)(expand_T *, int));
/// Returns a HashMap of variable_name -> full_declaration.
fn extract_static_funcptr_vars(filename: &str) -> FxHashMap<String, String> {
    use std::io::{BufRead, BufReader};
    use once_cell::sync::Lazy;

    let mut result: FxHashMap<String, String> = FxHashMap::default();

    let file = match File::open(filename) {
        Ok(f) => f,
        Err(_) => return result,
    };

    let reader = BufReader::new(file);

    // Pre-compile regex for static function pointer variable declarations
    // Pattern: static <type> *((*name)(<params>));
    // The key part is ((*name) which indicates a pointer to function
    static STATIC_FUNCPTR_RE: Lazy<regex::Regex> = Lazy::new(|| {
        // Matches: static ... ((*name)(...));
        // Group 1 captures the variable name
        regex::Regex::new(r"^static\s+.*\(\s*\(\s*\*\s*(\w+)\s*\)\s*\([^)]*\)\s*\)\s*;").unwrap()
    });

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };

        let trimmed = line.trim();

        // Skip preprocessor directives
        if trimmed.starts_with('#') {
            continue;
        }

        // Try to match static function pointer variable declaration
        if let Some(caps) = STATIC_FUNCPTR_RE.captures(trimmed) {
            if let Some(name_match) = caps.get(1) {
                let var_name = name_match.as_str().to_string();
                // Store the full declaration
                if !result.contains_key(&var_name) {
                    result.insert(var_name, trimmed.to_string());
                }
            }
        }
    }

    result
}

struct CommonDeclarations;

impl CommonDeclarations {
    // Identify declarations that appear in multiple PUs
    fn identify_common_deps(
        pu_order: &[String],
        uids: &FxHashMap<String, usize>,
        pids: &FxHashMap<String, usize>,
        dep: &FxHashMap<String, Vec<String>>,
        tags: &FxHashMap<String, Vec<String>>,
        _pu: &FxHashMap<String, String>,
        threshold: usize,  // Minimum number of PUs a declaration must appear in
    ) -> FxHashSet<String> {
        // Map: declaration -> count of PUs it appears in
        let mut dep_count: FxHashMap<String, usize> = FxHashMap::default();

        // For each PU, collect its necessary dependencies
        for u in pu_order.iter() {
            if !uids.contains_key(u) {
                continue;
            }

            let mut necessary: FxHashSet<String> = FxHashSet::default();
            necessary.insert(u.to_string());
            let j = *pids.get(u).unwrap();

            // Compute dependencies for this PU
            let mut processed: FxHashSet<String> = FxHashSet::default();
            let mut c = 1;
            while c > 0 {
                c = 0;
                for dep_u in pu_order[0..=j].iter().rev() {
                    if necessary.contains(dep_u) {
                        if let Some(deps) = dep.get(dep_u) {
                            let mut parts = dep_u.splitn(3, ':');
                            let name = if let (Some(_type_str), Some(n), Some(_file)) =
                                (parts.next(), parts.next(), parts.next()) {
                                n
                            } else {
                                continue;
                            };

                            for to in deps.iter() {
                                if to != name && !processed.contains(to) {
                                    if let Some(units) = tags.get(to) {
                                        for u_val in units {
                                            if !necessary.contains(u_val) {
                                                necessary.insert(u_val.to_string());
                                                c += 1;
                                            }
                                        }
                                    }
                                    processed.insert(to.to_string());
                                }
                            }
                        }
                    }
                }
            }

            // Count each necessary declaration for this PU
            for dep_u in &necessary {
                *dep_count.entry(dep_u.clone()).or_insert(0) += 1;
            }
        }

        // Collect declarations that appear in at least 'threshold' PUs
        let mut common: FxHashSet<String> = FxHashSet::default();
        for (decl, count) in dep_count.iter() {
            if *count >= threshold && !decl.contains("enumerator:") {
                // Only include type declarations (typedef, enum, struct) and extern variables
                // Don't include full function implementations in common header
                let parts: Vec<&str> = decl.split(":").collect();
                if parts.len() >= 1 {
                    let pu_type = PuType::from_str(parts[0]);
                    if pu_type.is_declaration() {
                        common.insert(decl.clone());
                    }
                }
            }
        }

        common
    }

    // Generate a common header file with shared declarations
    fn generate_common_header(
        filename: &str,
        common_deps: &FxHashSet<String>,
        pu: &FxHashMap<String, String>,
        pu_order: &[String],
    ) -> io::Result<String> {
        let header_filename = format!("{}_common.h", filename);

        let mut header_content = String::new();
        header_content.push_str(&format!("/* Common declarations for {} */\n", filename));
        header_content.push_str(&format!("#ifndef {}_COMMON_H\n", filename.to_uppercase().replace(".", "_")));
        header_content.push_str(&format!("#define {}_COMMON_H\n\n", filename.to_uppercase().replace(".", "_")));

        // Write common declarations in dependency order
        for u in pu_order.iter() {
            if common_deps.contains(u) {
                if let Some(code) = pu.get(u) {
                    let parts: Vec<&str> = u.split(":").collect();
                    let pu_type = PuType::from_str(parts[0]);

                    // For variables, convert to extern declarations
                    let decl_code = if pu_type.is_variable() {
                        convert_variable_to_declaration(code)
                    } else {
                        code.clone()
                    };

                    header_content.push_str(&decl_code);
                    if !decl_code.ends_with('\n') {
                        header_content.push('\n');
                    }
                }
            }
        }

        header_content.push_str(&format!("\n#endif /* {}_COMMON_H */\n",
            filename.to_uppercase().replace(".", "_")));

        // Write header file
        use std::fs;
        fs::write(&header_filename, &header_content)?;

        Ok(header_filename)
    }

    /// Generate a full PCH header for delta-PU compilation.
    /// Strategy:
    ///   1. Read the .i file directly up to the first function definition (the clean preamble)
    ///   2. Append forward declarations for ALL functions from pu_order
    /// Each delta PU file then only contains `#include "foo.pch.h"` + the function body.
    /// This reduces per-PU compile work from O(N) to O(1) → fresh build becomes O(N) total.
    fn generate_pch_header(
        filename: &str,
        pu_order: &[String],
        pu: &FxHashMap<String, String>,
        _system_typedefs: &[(String, String)],
        _project_types: &ProjectTypes,
    ) -> io::Result<(String, FxHashSet<String>)> {
        use std::fs;

        let pch_filename = format!("{}.pch.h", filename);
        let guard = std::path::Path::new(&pch_filename)
            .file_name()
            .map(|n| n.to_string_lossy().replace('.', "_").replace('-', "_").to_uppercase())
            .unwrap_or_else(|| "PCH_H".to_string());

        let mut content = String::with_capacity(1024 * 1024);
        content.push_str(&format!("#ifndef {}\n#define {}\n\n", guard, guard));

        // Part 1: Preamble extracted directly from the .i file (up to first function body)
        // This is the clean, verbatim content before any function definitions appear.
        // We find the first opening brace that belongs to a function (preceded by a line ending
        // with ')' — either K&R or Allman style).
        let raw = fs::read_to_string(filename)?;
        let file_lines: Vec<&str> = raw.lines().collect();
        // Find the first NON-INLINE function definition to determine preamble end.
        // Inline functions (static __inline__) are part of the preamble.
        // We track a small window of recent content lines to detect inline qualifiers.
        let mut preamble_end = file_lines.len(); // default: entire file
        let mut prev_ends_paren = false;
        let mut prev_paren_line_idx: usize = 0;
        let mut recent_content: Vec<String> = Vec::new(); // recent non-empty, non-linemarker lines
        for (i, line) in file_lines.iter().enumerate() {
            let s = line.trim();
            let is_fn_open = s.ends_with(") {") || s.ends_with("){")
                || s.ends_with(") __attribute__((noinline)) {")
                || (s == "{" && prev_ends_paren);
            if is_fn_open {
                // Check if inline function by looking at recent content lines
                let is_inline = recent_content.iter().rev().take(5).any(|l| {
                    l.contains("__inline") || l.contains("inline ")
                        || l.contains("__always_inline__")
                });
                if is_inline {
                    // Inline function — part of preamble, keep scanning
                    prev_ends_paren = false;
                    recent_content.clear();
                    continue;
                }
                // Non-inline function found — preamble ends here
                let sig_idx = if s == "{" && prev_ends_paren { prev_paren_line_idx } else { i };
                // Walk back to include return type line
                let mut fn_start = sig_idx;
                for back in (0..sig_idx).rev() {
                    let bl = file_lines[back].trim();
                    if bl.is_empty() { continue; }
                    if bl.starts_with('#') { continue; }
                    if bl.ends_with(';') || bl.ends_with('}') || bl == "{" { break; }
                    fn_start = back;
                    break;
                }
                preamble_end = fn_start;
                break;
            }
            if !s.is_empty() && !s.starts_with('#') {
                recent_content.push(s.to_string());
                if recent_content.len() > 8 { recent_content.remove(0); }
                if s.ends_with(')')
                    || s.ends_with("__attribute__((noinline))")
                    || s.ends_with("__attribute__((cold))")
                    || s.ends_with("__attribute__((hot))")
                {
                    prev_ends_paren = true;
                    prev_paren_line_idx = i;
                } else {
                    prev_ends_paren = false;
                }
            }
        }

        // Write the preamble lines, skipping GCC linemarkers (`# N "file" flags`)
        // Linemarkers cause re-processing of system headers when included as a .h file
        for line in file_lines[..preamble_end].iter() {
            let trimmed = line.trim_start();
            // Skip linemarkers: lines that start with `#` followed by a digit or space+digit
            // Format: `# <linenum> "<file>" [flags]` or `# <linenum> "<file>"`
            let is_linemarker = if trimmed.starts_with('#') {
                let rest = trimmed[1..].trim_start();
                rest.starts_with(|c: char| c.is_ascii_digit())
            } else {
                false
            };
            if is_linemarker {
                continue;
            }
            content.push_str(line);
            content.push('\n');
        }
        content.push('\n');

        // Part 2: Forward declarations for functions NOT already in the preamble.
        // Build a set of function names that appear in the preamble to avoid redefinition conflicts.
        let mut preamble_fn_names: FxHashSet<String> = FxHashSet::default();
        for line in file_lines[..preamble_end].iter() {
            let s = line.trim();
            if s.is_empty() || s.starts_with('#') { continue; }
            // Extract function name: look for `name(` pattern
            if let Some(paren_pos) = s.find('(') {
                let before_paren = s[..paren_pos].trim_end();
                let name = if let Some(sep) = before_paren.rfind(|c: char| !c.is_alphanumeric() && c != '_') {
                    &before_paren[sep + 1..]
                } else {
                    before_paren
                };
                if !name.is_empty() && name.starts_with(|c: char| c.is_alphabetic() || c == '_') {
                    preamble_fn_names.insert(name.to_string());
                }
            }
        }

        content.push_str("// Forward declarations for all functions\n");
        let mut emitted_func_keys: FxHashSet<String> = FxHashSet::default();
        for u in pu_order.iter() {
            if PuType::from_key(u) != PuType::Function {
                continue;
            }
            if !emitted_func_keys.insert(u.clone()) {
                continue;
            }
            // Skip functions already defined in the preamble (inline functions, etc.)
            let func_name = extract_key_name(u).unwrap_or("");
            if !func_name.is_empty() && preamble_fn_names.contains(func_name) {
                continue;
            }
            if let Some(code) = pu.get(u) {
                let decl = convert_function_to_declaration(code);
                let decl_trimmed = decl.trim();
                if !decl_trimmed.is_empty() {
                    content.push_str(&decl);
                    if !decl.ends_with('\n') {
                        content.push('\n');
                    }
                }
            }
        }

        content.push_str(&format!("\n#endif /* {} */\n", guard));
        fs::write(&pch_filename, &content)?;
        Ok((pch_filename, preamble_fn_names))
    }
}

pub fn dctags_process_file_direct(filename: &str) -> Result<(), String> {
    let dctags = DCTags::new()?;
    dctags.process_file_direct(filename)
}

fn is_problematic_basename(filename: &str) -> bool {
    // Known files that have issues with no-split mode processing
    // These will use passthrough mode to ensure 100% correctness
    // Exact basename matches (e.g. vim's "main.i", not "e1000_main.i")
    let problematic_basenames = [
        "main.i",
        "os_unix.i",
    ];
    let basename = std::path::Path::new(filename)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(filename);
    problematic_basenames.iter().any(|pattern| basename == *pattern)
}

// is_incomplete_preprocessed_file is now merged into scan_file_properties above.

/// Write a passthrough .pu.c file by streaming the input through a #line-directive filter.
/// `is_incomplete` is pre-computed by the caller (avoids a redundant file scan).
///
/// Optimizations vs old version:
/// - No `read_to_string` (no 4 MB heap String)
/// - No `Vec<&str>` + `join` in `strip_line_directives` (no 90K-entry Vec + second String)
/// - Streams input → output via BufReader/BufWriter (64 KB buffers, one syscall per buffer)
/// - `is_incomplete` supplied by caller — eliminates a duplicate file scan
fn passthrough_file(filename: &str, is_incomplete: bool) -> io::Result<()> {
    use std::io::{BufRead, BufReader, BufWriter, Write};
    use std::path::Path;

    let input  = std::fs::File::open(filename)?;
    let output_filename = format!("{}.pu.c", filename);
    let output = std::fs::File::create(&output_filename)?;

    let reader = BufReader::with_capacity(65536, input);
    let mut writer = BufWriter::with_capacity(65536, output);

    let mut first_line = true;
    for line_result in reader.lines() {
        let line = match line_result {
            Ok(l) => l,
            Err(e) => return Err(e),
        };
        let trimmed = line.trim_start();
        // Filter out #line directives (format: `# <digit> ...`)
        let is_line_directive = trimmed.starts_with('#') && {
            let rest = trimmed[1..].trim_start();
            rest.starts_with(|c: char| c.is_ascii_digit())
        };
        if is_line_directive {
            continue;
        }
        if !first_line {
            writer.write_all(b"\n")?;
        }
        writer.write_all(line.as_bytes())?;
        first_line = false;
    }
    // Preserve trailing newline if input ended with one (BufReader/lines() strips it,
    // so we always append one when we wrote at least one line — matches original behaviour).
    if !first_line {
        writer.write_all(b"\n")?;
    }
    writer.flush()?;

    let file_name = Path::new(filename).file_name().unwrap().to_str().unwrap();
    let reason = if is_incomplete {
        "incomplete preprocessed file - missing type definitions"
    } else {
        "known limitation"
    };
    eprintln!("Note: Using passthrough mode for {} ({})", file_name, reason);
    Ok(())
}

/// Extract simple type aliases from system headers
/// Only extracts single-line typedefs that alias basic/primitive C types DIRECTLY
/// (not stdint types which themselves require definitions)
/// Returns Vec of (typedef_name, full_typedef_line) for filtering against project definitions.
fn extract_system_typedefs(filename: &str) -> Vec<(String, String)> {
    use std::io::BufRead;

    let file = match std::fs::File::open(filename) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };

    let reader = std::io::BufReader::new(file);
    let mut typedefs = Vec::new();
    let mut in_system_header = false;
    let mut seen_names: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Only truly primitive C types - NOT stdint types which themselves need typedefs
    // These are the only types guaranteed to be available without any includes
    let primitive_types = [
        "char", "short", "int", "long", "float", "double", "void",
        "signed", "unsigned",
    ];

    // Types that look like they use primitives but actually reference other typedefs
    // (e.g., uint32_t, int64_t) - we must skip these
    let dependent_types = [
        "int8_t", "int16_t", "int32_t", "int64_t",
        "uint8_t", "uint16_t", "uint32_t", "uint64_t",
        "size_t", "ssize_t", "ptrdiff_t", "wchar_t",
        "intptr_t", "uintptr_t", "intmax_t", "uintmax_t",
    ];

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };

        // Track line markers to know if we're in a system header
        if line.starts_with("# ") || line.starts_with("#line ") {
            in_system_header = line.contains("/usr/") || line.contains("/lib/gcc/");
            continue;
        }

        // Extract only simple, complete single-line typedefs from system headers
        if in_system_header && line.trim().starts_with("typedef ") {
            let trimmed = line.trim();

            // Only accept complete single-line typedefs ending with ;
            // that don't define struct/union/enum bodies
            if trimmed.ends_with(';')
                && !trimmed.contains('{')
                && !trimmed.contains('}')
            {
                // Skip typedefs that reference internal types (starting with __)
                // or external library types (X11, Xt, etc.)
                if trimmed.contains("__") || trimmed.contains(" X")
                    || trimmed.contains("struct ") || trimmed.contains("union ")
                    || trimmed.contains("(*") // function pointers often have deps
                {
                    continue;
                }

                // Skip typedefs that reference other system typedefs (not truly primitive)
                let uses_dependent_type = dependent_types.iter().any(|dt| {
                    // Check if the typedef USES this type (not just defines it)
                    // e.g., "typedef uint32_t in_addr_t;" uses uint32_t
                    let words: Vec<&str> = trimmed.split_whitespace().collect();
                    // The dependent type would be after "typedef" but before the defined name
                    words.len() > 2 && words[1..words.len()-1].iter().any(|w| w.trim_matches(|c: char| !c.is_alphanumeric()) == *dt)
                });
                if uses_dependent_type {
                    continue;
                }

                // Check that the typedef only uses truly primitive types
                let is_primitive = primitive_types.iter().any(|pt| trimmed.contains(pt));
                if !is_primitive {
                    continue;
                }

                // Extract the typedef name
                if let Some(name) = extract_typedef_name(trimmed) {
                    if !seen_names.contains(&name) {
                        seen_names.insert(name.clone());
                        typedefs.push((name, trimmed.to_string()));
                    }
                }
            }
        }
    }

    typedefs
}

/// Extract the typedef name from a typedef declaration
/// Returns the name being defined (rightmost identifier before semicolon)
fn extract_typedef_name(typedef_line: &str) -> Option<String> {
    // Remove the trailing semicolon and any trailing whitespace
    let s = typedef_line.trim_end_matches(';').trim();

    // Handle array declarations: typedef int arr[10]; -> "arr"
    // Remove array brackets from the end
    let s = if let Some(bracket_pos) = s.rfind('[') {
        s[..bracket_pos].trim()
    } else {
        s
    };

    // Find the last word-like token (alphanumeric + underscore)
    // This handles: typedef int foo; typedef void (*funcptr)(int);
    let mut end = s.len();
    let start;

    let chars: Vec<char> = s.chars().collect();
    let mut i = chars.len();

    // Skip backwards to find the end of the identifier
    while i > 0 {
        i -= 1;
        let c = chars[i];
        if c.is_alphanumeric() || c == '_' {
            end = i + 1;
            break;
        }
    }

    // Find the start of the identifier
    while i > 0 {
        let c = chars[i - 1];
        if c.is_alphanumeric() || c == '_' {
            i -= 1;
        } else {
            break;
        }
    }
    start = i;

    if start < end {
        Some(s[start..end].to_string())
    } else {
        None
    }
}

/// Bug46: Extract the struct/union name being defined in a typedef or struct declaration
/// For "typedef struct __X { ... } Y;" returns Some("__X")
/// For "struct __X { ... };" returns Some("__X")
/// Returns None if no struct/union name with __ prefix is found
fn extract_defining_struct_name(code: &str) -> Option<String> {
    // Look for "typedef struct __name" or "typedef union __name" or "struct __name" or "union __name"
    let patterns = ["typedef struct ", "typedef union ", "struct ", "union "];

    for pattern in patterns.iter() {
        if let Some(start_pos) = code.find(pattern) {
            let after_keyword = &code[start_pos + pattern.len()..];
            // Extract the struct/union name (first identifier)
            let name: String = after_keyword
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();

            // Only return if it's an internal name (starts with __)
            if name.starts_with("__") {
                return Some(name);
            }
        }
    }
    None
}

/// Bug46: Check if body contains references to external internal structs/unions
/// A reference is "external" if it's not the same as the struct being defined
/// For example, in "typedef struct __X { struct __X *ptr; } Y;", the "struct __X"
/// inside the body is self-referential (not external), so we return false.
/// But in "typedef union { struct __Y data; } Z;", the "struct __Y" is external.
fn has_external_internal_struct_ref(body: &str, defining_name: Option<&str>) -> bool {
    // Find all "struct __" or "union __" patterns in the body
    // Note: pattern ends with just "struct " or "union ", then we check for "__" separately
    let patterns = [("struct ", "struct"), ("union ", "union")];

    for (pattern, _kind) in patterns.iter() {
        let mut search_pos = 0;
        while let Some(found_pos) = body[search_pos..].find(pattern) {
            let abs_pos = search_pos + found_pos;
            let after_keyword = &body[abs_pos + pattern.len()..];

            // Extract the struct/union name being referenced (first identifier)
            let ref_name: String = after_keyword
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();

            // Only care about names starting with "__" (internal names)
            if ref_name.starts_with("__") {
                // Compare with defining_name (if provided)
                match defining_name {
                    Some(def_name) => {
                        // If the reference is to a different internal struct, it's external
                        if ref_name != def_name {
                            return true; // External reference found
                        }
                        // Otherwise, it's a self-reference, which is OK
                    }
                    None => {
                        // No defining name provided, so any __ reference is external
                        return true;
                    }
                }
            }

            search_pos = abs_pos + pattern.len();
        }
    }

    false // No external references found
}

/// Extract function/type bodies from the source file using ctags line numbers.
/// For each unit in pu_order with an empty body and a known line number,
/// reads from that line to the matching closing brace and stores the result.
fn fill_bodies_from_line_numbers(filename: &str) {
    use std::io::BufRead;

    // Collect units that need body extraction (empty pu entries with known line numbers)
    let units_to_fill: Vec<(String, u64)> = with_tag_info(|tag_info| {
        tag_info.pu_order.iter()
            .filter_map(|u| {
                let needs_fill = tag_info.pu.get(u).map_or(true, |v| {
                    if v.is_empty() {
                        return true;
                    }
                    // For typedef/struct/union entries, if body is only a closing line
                    // (starts with '}'), re-fill from source to get the complete definition.
                    // This fixes cases where ctags reports only "} TypeName;" but the full
                    // struct definition lives on earlier lines.
                    let is_type_entry = u.starts_with("typedef:")
                        || u.starts_with("struct:")
                        || u.starts_with("union:");
                    if is_type_entry {
                        let trimmed = v.trim_start();
                        // Only re-fill if the body is a single closing line (no opening brace)
                        trimmed.starts_with('}') && !trimmed.contains('{')
                    } else if u.starts_with("function:") {
                        // Bug-frag fix: For function entries, if the body starts with
                        // "funcname(" (no return type on the same line), the return type
                        // is on the previous source line (K&R multi-line style).
                        // Re-fill from source to include the return type.
                        // Extract func name from key "function:funcname:file"
                        if let Some(rest) = u.strip_prefix("function:") {
                            if let Some(colon) = rest.find(':') {
                                let func_name = &rest[..colon];
                                let trimmed = v.trim_start();
                                // Body starts with "funcname(" — missing return type
                                trimmed.starts_with(&format!("{}(", func_name))
                            } else {
                                false
                            }
                        } else {
                            false
                        }
                    } else {
                        false
                    }
                });
                if needs_fill {
                    tag_info.line_numbers.get(u).map(|&ln| (u.clone(), ln))
                } else {
                    None
                }
            })
            .collect()
    });

    if DEBUG_TAGS_ENABLED.load(Ordering::Relaxed) {
        eprintln!("DEBUG fill_bodies: units_to_fill={}, filename={}", units_to_fill.len(), filename);
        for (u, ln) in &units_to_fill {
            eprintln!("  fill: u={} line={}", u, ln);
        }
        with_tag_info(|ti| {
            eprintln!("  line_numbers map has {} entries", ti.line_numbers.len());
            for (k, v) in ti.line_numbers.iter().take(5) {
                eprintln!("    {}={}", k, v);
            }
        });
    }

    if units_to_fill.is_empty() {
        return;
    }

    // Read the source file into lines (1-indexed by converting to Vec)
    let file = match std::fs::File::open(filename) {
        Ok(f) => f,
        Err(_) => return,
    };
    let reader = std::io::BufReader::new(file);
    let source_lines: Vec<String> = reader.lines()
        .map(|l| l.unwrap_or_default())
        .collect();

    let total_lines = source_lines.len();

    for (u, start_line) in units_to_fill {
        if start_line == 0 || start_line as usize > total_lines {
            continue;
        }

        let mut start_idx = (start_line - 1) as usize; // 0-based index


        // If ctags reported the CLOSING line of a typedef struct (line starts with '}'),
        // scan backwards to find the matching opening '{' line, then go further back
        // to find the 'typedef struct' keyword line.
        if source_lines[start_idx].trim_start().starts_with('}') {
            // Special case: for anonymous enums (enum:__anon*), when ctags reports the
            // closing '}; 'of the PREVIOUS anonymous enum as the start of the NEXT one,
            // we should look FORWARD for the next 'enum {' declaration instead of
            // scanning backward (which would capture the wrong enum's body).
            // This happens because dctags assigns __anonN to the next anonymous enum
            // but reports the line number where the previous enum's '}' appears.
            let is_anon_enum = u.contains(":__anon");
            if is_anon_enum {
                // Scan forward from start_idx+1 to find the next 'enum' keyword
                let mut forward_found = false;
                for fwd_idx in (start_idx + 1)..total_lines {
                    let trimmed = source_lines[fwd_idx].trim();
                    if trimmed.is_empty() || trimmed.starts_with('#') {
                        continue;
                    }
                    // Look for a line that starts with 'enum' (not typedef)
                    if trimmed.starts_with("enum") {
                        start_idx = fwd_idx;
                        forward_found = true;
                        break;
                    }
                    // If we hit a typedef or struct before finding enum, stop
                    if trimmed.starts_with("typedef") || trimmed.starts_with("struct")
                        || trimmed.starts_with("union") || trimmed.starts_with("static")
                        || trimmed.starts_with("extern") || (trimmed.contains('(') && !trimmed.starts_with("/*")) {
                        break;
                    }
                }
                if !forward_found {
                    // Fallback: scan backward as before
                    let mut net: i32 = 0;
                    let mut open_brace_idx = start_idx;
                    'back2: for back_idx in (0..=start_idx).rev() {
                        for c in source_lines[back_idx].chars() {
                            match c {
                                '}' => net += 1,
                                '{' => {
                                    net -= 1;
                                    if net == 0 {
                                        open_brace_idx = back_idx;
                                        break 'back2;
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    let mut decl_start = open_brace_idx;
                    for back_idx in (0..open_brace_idx).rev() {
                        let trimmed = source_lines[back_idx].trim();
                        if trimmed.is_empty() { break; }
                        if trimmed.starts_with('#') { break; }
                        if trimmed.contains('{') { break; }
                        if trimmed.starts_with("typedef") || trimmed.starts_with("struct")
                            || trimmed.starts_with("enum") || trimmed.starts_with("union") {
                            decl_start = back_idx;
                        }
                    }
                    start_idx = decl_start;
                }
            } else {
                // Count net braces scanning from closing line upward.
                // We want to find where braces balance: each '}' adds 1, each '{' subtracts 1.
                // When count reaches 0 after seeing braces, we found the matching '{'.
                let mut net: i32 = 0;
                let mut open_brace_idx = start_idx;
                'back: for back_idx in (0..=start_idx).rev() {
                    for c in source_lines[back_idx].chars() {
                        match c {
                            '}' => net += 1,
                            '{' => {
                                net -= 1;
                                if net == 0 {
                                    open_brace_idx = back_idx;
                                    break 'back;
                                }
                            }
                            _ => {}
                        }
                    }
                }
                // Now scan further back from the opening brace to find 'typedef struct' or 'struct'
                let mut decl_start = open_brace_idx;
                for back_idx in (0..open_brace_idx).rev() {
                    let trimmed = source_lines[back_idx].trim();
                    if trimmed.is_empty() {
                        break; // stop at blank line
                    }
                    // Stop at preprocessor directives (they separate different declarations)
                    if trimmed.starts_with('#') {
                        break;
                    }
                    // Stop before going past another open brace (don't include previous struct body)
                    if trimmed.contains('{') {
                        break;
                    }
                    if trimmed.starts_with("typedef") || trimmed.starts_with("struct")
                        || trimmed.starts_with("enum") || trimmed.starts_with("union") {
                        decl_start = back_idx;
                    }
                }
                start_idx = decl_start;
            }
        }

        // For anonymous enums where the start line doesn't contain '{' (ctags reported
        // a non-enum line as the start), scan forward to find the actual 'enum' keyword.
        // This handles cases where dctags assigns a line number before the enum's '{'.
        if u.contains(":__anon") && !source_lines[start_idx].contains('{') && !source_lines[start_idx].trim_start().starts_with('}') {
            for fwd_idx in start_idx..total_lines {
                let trimmed = source_lines[fwd_idx].trim();
                if trimmed.starts_with("enum") {
                    start_idx = fwd_idx;
                    break;
                }
                // Stop if we hit something that would be a different declaration
                if fwd_idx > start_idx && (trimmed.starts_with("typedef") || trimmed.starts_with("struct")
                    || trimmed.starts_with("union") || trimmed.starts_with("static")
                    || trimmed.starts_with("extern")) {
                    break;
                }
            }
        }

        // Bug-frag fix: For function entries, if the start line begins with the function name
        // (no return type on that line), check if the previous source line is the return type.
        // This handles K&R multi-line style like:
        //   static Frag_T
        //   frag(nfa_state_T *start, Ptrlist *out) { ... }
        if u.starts_with("function:") && start_idx > 0 {
            let start_line_trimmed = source_lines[start_idx].trim();
            // Extract function name from key
            if let Some(rest) = u.strip_prefix("function:") {
                if let Some(colon) = rest.find(':') {
                    let func_name = &rest[..colon];
                    // If start line begins with "funcname(" or "*funcname("
                    if start_line_trimmed.starts_with(&format!("{}(", func_name))
                        || start_line_trimmed.starts_with(&format!("*{}(", func_name))
                    {
                        // Look back for the return type line
                        let prev_line = source_lines[start_idx - 1].trim();
                        // The previous line should be a type (not a brace, comment, preprocessor, or empty)
                        if !prev_line.is_empty()
                            && !prev_line.starts_with('#')
                            && !prev_line.starts_with("/*")
                            && !prev_line.starts_with("//")
                            && !prev_line.contains('{')
                            && !prev_line.contains('}')
                            && !prev_line.contains(';')
                        {
                            start_idx -= 1;
                        }
                    }
                }
            }
        }

        // Extract from start_line until matching closing brace
        // Track brace depth; also handle string/char literals and comments
        let mut body = String::new();
        let mut brace_depth: i32 = 0;
        let mut found_open = false;
        let mut in_line_comment = false;
        let mut in_block_comment = false;
        let mut in_string = false;
        let mut in_char = false;
        let mut prev_char = '\0';

        'outer: for line_idx in start_idx..total_lines {
            let line = &source_lines[line_idx];

            // Reset line-comment state at start of each line
            in_line_comment = false;
            body.push_str(line);
            body.push('\n');

            let chars: Vec<char> = line.chars().collect();
            let mut ci = 0;
            while ci < chars.len() {
                let c = chars[ci];

                if in_line_comment {
                    // rest of line is comment
                    break;
                }

                if in_block_comment {
                    if prev_char == '*' && c == '/' {
                        in_block_comment = false;
                    }
                    prev_char = c;
                    ci += 1;
                    continue;
                }

                if in_string {
                    if c == '"' {
                        // Count preceding backslashes to determine if quote is escaped
                        // Even number of backslashes = closing quote; odd = escaped quote
                        let mut num_backslashes = 0;
                        let mut bi = ci;
                        while bi > 0 && chars[bi - 1] == '\\' {
                            num_backslashes += 1;
                            bi -= 1;
                        }
                        if num_backslashes % 2 == 0 {
                            in_string = false;
                        }
                    }
                    prev_char = c;
                    ci += 1;
                    continue;
                }

                if in_char {
                    if c == '\'' {
                        // Count preceding backslashes
                        let mut num_backslashes = 0;
                        let mut bi = ci;
                        while bi > 0 && chars[bi - 1] == '\\' {
                            num_backslashes += 1;
                            bi -= 1;
                        }
                        if num_backslashes % 2 == 0 {
                            in_char = false;
                        }
                    }
                    prev_char = c;
                    ci += 1;
                    continue;
                }

                match c {
                    '/' if ci + 1 < chars.len() && chars[ci + 1] == '/' => {
                        in_line_comment = true;
                    }
                    '/' if ci + 1 < chars.len() && chars[ci + 1] == '*' => {
                        in_block_comment = true;
                        prev_char = c;
                        ci += 1;
                    }
                    '"' => { in_string = true; }
                    '\'' => { in_char = true; }
                    '{' => {
                        brace_depth += 1;
                        found_open = true;
                    }
                    '}' => {
                        if found_open {
                            brace_depth -= 1;
                            if brace_depth == 0 {
                                break 'outer;
                            }
                        }
                    }
                    ';' => {
                        // For declarations without braces (typedef int foo;, extern int x;)
                        // stop at the first semicolon at depth 0 when no brace was seen yet.
                        if !found_open && brace_depth == 0 {
                            break 'outer;
                        }
                    }
                    _ => {}
                }

                prev_char = c;
                ci += 1;
            }
        }

        // Store body if: we found a brace-enclosed body, OR we captured a simple
        // single-line/semicolon-terminated declaration (found_open=false but body non-empty)
        if !body.is_empty() && (found_open || body.trim().ends_with(';')) {
            with_tag_info(|tag_info| {
                tag_info.pu.insert(u.clone(), body.clone());
            });
        }
    }
}

/// Scan the file for function definitions that ctags missed (e.g., due to early abort).
/// Returns `true` if any new entries were added.
///
/// Strategy: find the highest line number ctags reported, then scan lines after that
/// for C function definitions using a simple pattern matcher that handles GCC-style
/// declarations (attribute, static, inline prefixes).
fn scan_uncovered_functions(filename: &str) -> bool {
    use std::io::BufRead;

    // Check how many lines ctags covered vs total lines in file
    let max_ctags_line: u64 = with_tag_info(|ti| {
        ti.line_numbers.values().copied().max().unwrap_or(0)
    });
    if max_ctags_line == 0 {
        return false;
    }

    // Count total lines
    let file = match std::fs::File::open(filename) {
        Ok(f) => f,
        Err(_) => return false,
    };
    let reader = std::io::BufReader::new(file);
    let source_lines: Vec<String> = reader.lines().map(|l| l.unwrap_or_default()).collect();
    let total_lines = source_lines.len() as u64;

    // Only scan if ctags covered less than 90% of the file
    if total_lines == 0 || max_ctags_line as f64 / total_lines as f64 > 0.90 {
        return false;
    }

    // Scan from just after max_ctags_line to end of file
    let scan_start = max_ctags_line as usize; // 0-indexed = line after last ctags line

    // Collect existing names to avoid duplicates
    let existing_names: std::collections::HashSet<String> = with_tag_info(|ti| {
        ti.pu_order.iter()
            .filter_map(|u| {
                let mut parts = u.splitn(3, ':');
                parts.next(); // type
                parts.next().map(|n| n.to_string())
            })
            .collect()
    });

    // Track the current source file name from #line directives
    let mut current_source_file = filename.to_string();
    let mut new_entries: Vec<(String, String, u64, String)> = Vec::new(); // (name, file, line, body)

    let mut i = scan_start;
    while i < source_lines.len() {
        let line = &source_lines[i];

        // Track #line directives: `# N "file.c"` or `# N`
        if line.starts_with('#') {
            let rest = line.trim_start_matches('#').trim_start();
            if let Some(after_num) = rest.chars().next().filter(|c| c.is_ascii_digit()).and_then(|_| {
                // Skip digits
                let end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
                Some(&rest[end..])
            }) {
                let trimmed = after_num.trim();
                if trimmed.starts_with('"') {
                    // Extract filename
                    let fname = trimmed.trim_start_matches('"');
                    if let Some(end) = fname.find('"') {
                        current_source_file = fname[..end].to_string();
                    }
                }
            }
            i += 1;
            continue;
        }

        // Check if this line looks like a function definition start.
        // We look for a line that:
        // 1. Is not empty, not a preprocessor directive
        // 2. Contains `identifier(` pattern
        // 3. Is followed (possibly after one blank line) by `{`
        if is_function_def_start(line) {
            let fn_name = extract_function_name(line);
            if !fn_name.is_empty() && !existing_names.contains(&fn_name) {
                // Look for the opening brace on the next few lines.
                // Multi-line signatures like:
                //   static int foo(struct bar *x,
                //                  int y)
                //   {
                // are handled by allowing continuation lines (parameter lists, etc.)
                let mut brace_line = i + 1;
                let mut paren_depth: i32 = line.chars().filter(|&c| c == '(').count() as i32
                    - line.chars().filter(|&c| c == ')').count() as i32;
                while brace_line < source_lines.len() && brace_line < i + 12 {
                    let bl = source_lines[brace_line].trim();
                    if bl.starts_with('{') {
                        // Found the opening brace - extract body
                        let body = extract_body_from_lines(&source_lines, i, brace_line);
                        if !body.is_empty() {
                            new_entries.push((fn_name.clone(), current_source_file.clone(), (i + 1) as u64, body));
                        }
                        break;
                    } else if bl.is_empty() || bl.starts_with('#') {
                        // blank / directive lines are fine — keep scanning
                    } else if paren_depth > 0 {
                        // Inside an open paren — this is a parameter continuation line
                        paren_depth += bl.chars().filter(|&c| c == '(').count() as i32;
                        paren_depth -= bl.chars().filter(|&c| c == ')').count() as i32;
                    } else {
                        break; // Unexpected non-brace content after closed parens
                    }
                    brace_line += 1;
                }
            }
        }
        i += 1;
    }

    if new_entries.is_empty() {
        return false;
    }

    let added = new_entries.len();
    with_tag_info(|tag_info| {
        for (name, file, line_no, body) in new_entries {
            let u = make_unit_key("function", &name, &file);
            if !tag_info.pu_order_set.contains(&u) {
                tag_info.pu_order.push(u.clone());
                tag_info.pu_order_set.insert(u.clone());
                tag_info.pu.insert(u.clone(), body);
                tag_info.line_numbers.insert(u.clone(), line_no);
                tag_info.tags.entry(name.clone())
                    .or_default()
                    .push(format!("function:{}", file));
            }
        }
    });

    added > 0
}

/// Check if a source line looks like the start of a C function definition
/// (not a declaration/prototype, not a preprocessor line, not a comment)
fn is_function_def_start(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with("//") || trimmed.starts_with("/*") {
        return false;
    }
    // Must contain an identifier followed by '(' — but NOT end with ';' (that would be a prototype)
    if trimmed.ends_with(';') {
        return false;
    }
    // Must contain '(' indicating a function call-ish syntax
    if !trimmed.contains('(') {
        return false;
    }
    // Skip lines that are just expressions/assignments
    if trimmed.starts_with("return ") || trimmed.starts_with("if ") || trimmed.starts_with("while ")
        || trimmed.starts_with("for ") || trimmed.starts_with("do ") || trimmed.starts_with("switch ")
    {
        return false;
    }
    // The line should contain a word that could be a function name followed by '('
    // Look for identifier(...) pattern where identifier is not a keyword
    let skip_keywords = ["if", "while", "for", "switch", "return", "sizeof", "typeof",
        "__typeof__", "__attribute__", "__asm__", "__asm", "asm", "do", "else",
        "_Generic", "__builtin_expect", "__builtin"];
    // Find the function name candidate: last identifier before '('
    if let Some(name) = extract_function_name(line).as_str().chars().next() {
        if name.is_alphabetic() || name == '_' {
            let name_str = extract_function_name(line);
            if skip_keywords.contains(&name_str.as_str()) {
                return false;
            }
            return true;
        }
    }
    false
}

/// Extract the function name from a C function definition line.
/// Handles patterns like:
///   `static int e1000_probe(...)`
///   `void e1000_remove(struct pci_dev *pdev)`
///   `static __attribute__((noinline)) int foo(void)`
fn extract_function_name(line: &str) -> String {
    // Find the '(' position
    let paren_pos = match line.find('(') {
        Some(p) => p,
        None => return String::new(),
    };

    // Get text before '(' and find the last identifier
    let before_paren = &line[..paren_pos];
    let mut name = String::new();
    let mut in_name = false;

    for ch in before_paren.chars().rev() {
        if ch.is_alphanumeric() || ch == '_' {
            in_name = true;
            name.push(ch);
        } else if in_name {
            break;
        }
    }

    // Name was built in reverse
    name.chars().rev().collect()
}

/// Extract the full body of a function starting from declaration_line through
/// the opening brace at brace_line, until the matching closing brace.
fn extract_body_from_lines(lines: &[String], decl_line: usize, brace_line: usize) -> String {
    let total = lines.len();
    let mut body = String::new();

    // Include all lines from decl_line through the closing brace
    let mut depth: i32 = 0;
    let mut in_string = false;
    let mut in_char = false;
    let mut in_line_comment = false;
    let mut in_block_comment = false;
    let mut prev_char = '\0';
    let mut found_open = false;

    'outer: for line_idx in decl_line..total {
        let line = &lines[line_idx];
        in_line_comment = false;
        body.push_str(line);
        body.push('\n');

        let chars: Vec<char> = line.chars().collect();
        let mut ci = 0;
        while ci < chars.len() {
            let c = chars[ci];
            // Block comment handling
            if in_block_comment {
                if prev_char == '*' && c == '/' {
                    in_block_comment = false;
                }
                prev_char = c;
                ci += 1;
                continue;
            }
            // Line comment
            if in_line_comment {
                prev_char = c;
                ci += 1;
                continue;
            }
            // String literal
            if in_string {
                if c == '"' && prev_char != '\\' {
                    in_string = false;
                }
                prev_char = c;
                ci += 1;
                continue;
            }
            // Char literal
            if in_char {
                if c == '\'' && prev_char != '\\' {
                    in_char = false;
                }
                prev_char = c;
                ci += 1;
                continue;
            }
            // Start of comment/string
            if c == '/' && ci + 1 < chars.len() {
                if chars[ci + 1] == '/' {
                    in_line_comment = true;
                    prev_char = c;
                    ci += 1;
                    continue;
                } else if chars[ci + 1] == '*' {
                    in_block_comment = true;
                    prev_char = c;
                    ci += 1;
                    continue;
                }
            }
            if c == '"' { in_string = true; }
            else if c == '\'' { in_char = true; }
            else if c == '{' {
                depth += 1;
                found_open = true;
            } else if c == '}' {
                depth -= 1;
                if found_open && depth == 0 {
                    prev_char = c;
                    break 'outer;
                }
            }
            prev_char = c;
            ci += 1;
        }
    }

    if found_open && depth == 0 {
        body
    } else {
        String::new()
    }
}

/// Extract glibc internal typedefs from system headers in preprocessed file
/// These are typedefs like __uint16_t, __uint32_t that use only primitive types
fn extract_glibc_internal_typedefs(filename: &str) -> Vec<(String, String)> {
    use std::io::BufRead;

    let file = match std::fs::File::open(filename) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };

    let reader = std::io::BufReader::new(file);
    let mut typedefs = Vec::new();
    let mut in_system_header = false;
    let mut seen_names: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut line_num = 0usize;
    let debug = std::env::var("DEBUG_TYPEDEFS").is_ok();

    // Only truly primitive C types
    let primitive_tokens = [
        "char", "short", "int", "long", "float", "double", "void",
        "signed", "unsigned",
    ];

    for line in reader.lines() {
        line_num += 1;
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };

        // Track line markers to know if we're in a system header
        if line.starts_with("# ") || line.starts_with("#line ") {
            in_system_header = line.contains("/usr/") || line.contains("/lib/gcc/");
            if debug && line.contains("__uint16_t") {
                eprintln!("DEBUG: Line {}: marker with __uint16_t: {}", line_num, &line[..line.len().min(80)]);
            }
            continue;
        }

        // Debug: track when we see __uint16_t
        if debug && line.contains("__uint16_t") {
            eprintln!("DEBUG: Line {}: __uint16_t found, in_system_header={}: {}", line_num, in_system_header, &line[..line.len().min(80)]);
        }

        // Extract glibc internal typedefs (__uint16_t, __uint32_t, etc.)
        if in_system_header && line.trim().starts_with("typedef ") {
            let trimmed = line.trim();

            // Only accept complete single-line typedefs ending with ;
            // that define __-prefixed types using only primitive types
            if trimmed.ends_with(';')
                && !trimmed.contains('{')
                && !trimmed.contains('}')
                && !trimmed.contains("struct ")
                && !trimmed.contains("union ")
                && !trimmed.contains("(*")  // skip function pointers
            {
                // Extract the typedef name
                if let Some(name) = extract_typedef_name(trimmed) {
                    if !seen_names.contains(&name) {
                        // Verify the typedef only uses primitive types or already-seen types
                        // Extract the part between "typedef" and the name
                        let without_typedef = trimmed.strip_prefix("typedef ").unwrap_or(trimmed);
                        let name_start = without_typedef.rfind(&name).unwrap_or(0);
                        let type_part = &without_typedef[..name_start];

                        // Check that type_part only contains primitive tokens, whitespace,
                        // or already-captured system types (so we avoid forward refs to structs)
                        let tokens: Vec<&str> = type_part.split_whitespace().collect();
                        let all_primitive = tokens.iter().all(|t| {
                            primitive_tokens.contains(t) || t.is_empty()
                            || seen_names.contains(*t)
                        });

                        if debug && name == "__uint16_t" {
                            eprintln!("DEBUG: Line {}: name={}, type_part='{}', tokens={:?}, all_primitive={}",
                                     line_num, name, type_part, tokens, all_primitive);
                        }

                        if all_primitive && !tokens.is_empty() {
                            if debug && name == "__uint16_t" {
                                eprintln!("DEBUG: Line {}: CAPTURED __uint16_t!", line_num);
                            }
                            seen_names.insert(name.clone());
                            typedefs.push((name, trimmed.to_string()));
                        }
                    }
                } else if debug && trimmed.contains("__uint16_t") {
                    eprintln!("DEBUG: Line {}: extract_typedef_name returned None for: {}", line_num, trimmed);
                }
            } else if debug && trimmed.contains("__uint16_t") {
                eprintln!("DEBUG: Line {}: failed filter checks for: {}", line_num, trimmed);
            }
        }
    }

    // Detect stdbool-style definitions: `typedef _Bool bool;` and `enum { false = 0, true = 1 }`.
    // These appear in kernel headers (and any code that rolls its own stdbool) and must be treated
    // as system-level — emitted once at the top of each .pu.c rather than re-emitted per function.
    //
    // Strategy: if we saw these patterns anywhere in the file, inject synthetic system-typedef
    // entries so ProjectTypes::build excludes them from project types, and the system_typedefs
    // path (line ~6086) emits them exactly once per .pu.c file.
    //
    // We collect the actual text so the declaration style matches the source.
    let mut found_bool_typedef: Option<String> = None;
    let mut found_bool_enum: Option<String> = None;

    // Re-scan for stdbool patterns (a second linear pass is cheap vs the alternative)
    if let Ok(f2) = std::fs::File::open(filename) {
        let reader2 = std::io::BufReader::new(f2);
        let mut collecting_enum = false;
        let mut enum_buf = String::new();
        for line in reader2.lines().flatten() {
            let trimmed = line.trim();
            // typedef _Bool bool;  (common kernel/glibc stdbool pattern)
            if found_bool_typedef.is_none()
                && trimmed.starts_with("typedef")
                && trimmed.contains("_Bool")
                && trimmed.contains("bool")
                && trimmed.ends_with(';')
            {
                found_bool_typedef = Some(trimmed.to_string());
            }
            // enum { false = 0, true = 1 };  (may span a single line or multiple lines)
            if found_bool_enum.is_none() {
                if !collecting_enum {
                    if trimmed.starts_with("enum") && (trimmed.contains("false") || trimmed.contains("true")) {
                        if trimmed.ends_with(';') {
                            found_bool_enum = Some(trimmed.to_string());
                        } else {
                            collecting_enum = true;
                            enum_buf = trimmed.to_string();
                        }
                    }
                } else {
                    enum_buf.push(' ');
                    enum_buf.push_str(trimmed);
                    if trimmed.ends_with(';') {
                        collecting_enum = false;
                        if enum_buf.contains("false") && enum_buf.contains("true") {
                            found_bool_enum = Some(enum_buf.clone());
                        }
                        enum_buf.clear();
                    }
                }
            }
        }
    }

    // Inject stdbool entries as synthetic system typedefs (dedup by seen_names already populated)
    // Order matters: enum { false, true } must come before typedef _Bool bool if both present,
    // because bool may depend on _Bool which is primitive — actually typedef comes first
    // to match typical stdbool.h ordering.
    if let Some(ref bool_typedef) = found_bool_typedef {
        if !seen_names.contains("bool") {
            seen_names.insert("bool".to_string());
            // Also register _Bool as known
            seen_names.insert("_Bool".to_string());
            typedefs.push(("bool".to_string(), bool_typedef.clone()));
        }
    }
    if let Some(ref bool_enum) = found_bool_enum {
        if !seen_names.contains("false") {
            seen_names.insert("false".to_string());
            seen_names.insert("true".to_string());
            typedefs.push(("false".to_string(), bool_enum.clone()));
        }
    }

    typedefs
}

// ============================================================================
// Cluster-PCH mode: dependency-graph-aware clustering for header-heavy files
//
// For files like Linux kernel .i files where src_frac is very low (1-5%),
// the monolithic PCH approach fails because:
//   1. The single PCH covers ALL headers (60K+ lines of kernel headers)
//   2. The PCH may fail to precompile due to arch-specific inline asm
//   3. Even if it compiles, each bundle re-links the full .gch
//
// This mode instead:
//   1. Uses the existing TransitiveDeps bitvectors to find which header-origin
//      PUs each function depends on
//   2. Groups functions by their header-file fingerprint (set of .h files in deps)
//   3. For each cluster, scans only the relevant sections of the .i file to
//      produce a minimal PCH header (much smaller than the full-file PCH)
//   4. Writes one bundle per cluster: #include "cluster_N.pch.h" + function bodies
//
// Result: instead of 800 × (full header parse), we get K clusters × (partial parse)
// where K is small (typically 3-10 for kernel files) and each partial header is
// much smaller than the full kernel header tree.
// ============================================================================

/// Cluster functions by their transitive dependency set similarity.
///
/// For kernel files all functions share the same top-level headers (via one #include chain),
/// so clustering by header file names gives only 1 cluster. Instead we cluster by the
/// specific PU symbols (structs, typedefs, enums) each function actually uses, which
/// differs between functions even when they share the same headers.
///
/// Algorithm: spectral-lite clustering using bitvector dot-product similarity.
///   1. For each function, extract its dep bitvector (which symbols it uses)
///   2. Compute N "centroid" bitvectors by sampling N evenly-spaced functions as seeds
///   3. Assign each function to the centroid with highest overlap (most shared symbols)
///   4. Iterate until stable (or max_iter rounds)
///
/// This produces clusters of functions that share a common symbol subset → they can
/// share a minimal PCH covering only those symbols.
fn cluster_functions_by_headers(
    fn_keys: &[&String],
    transitive_deps: &TransitiveDeps,
    max_clusters: usize,
) -> Vec<Vec<String>> {
    if fn_keys.is_empty() {
        return Vec::new();
    }

    let n_fns = fn_keys.len();
    let words = transitive_deps.words_per_node;

    // Get bitvector for each function from TransitiveDeps
    let fn_bvs: Vec<Option<&Vec<u64>>> = fn_keys.iter()
        .map(|k| transitive_deps.key_index.get_idx(k)
            .and_then(|idx| transitive_deps.bitvecs.get(idx as usize)))
        .collect();

    // Count functions with valid bitvectors
    let valid_count = fn_bvs.iter().filter(|b| b.is_some()).count();
    if valid_count == 0 || words == 0 {
        // No dependency info available — put everything in one cluster
        return vec![fn_keys.iter().map(|k| (*k).clone()).collect()];
    }

    let k = max_clusters.min(n_fns);

    // Seed centroids: pick k evenly-spaced functions as initial centroids
    let mut centroids: Vec<Vec<u64>> = (0..k)
        .map(|i| {
            let idx = (i * n_fns) / k;
            fn_bvs[idx].cloned().unwrap_or_else(|| vec![0u64; words])
        })
        .collect();

    // Bitvector popcount (number of set bits)
    let popcount = |bv: &[u64]| -> u64 {
        bv.iter().map(|w| w.count_ones() as u64).sum()
    };

    // Jaccard-like similarity: |A ∩ B| / max(|A|, |B|)
    // Using bitwise AND for intersection, then popcount
    let similarity = |a: &[u64], b: &[u64]| -> u64 {
        let inter: u64 = a.iter().zip(b.iter()).map(|(x, y)| (x & y).count_ones() as u64).sum();
        let pa = popcount(a);
        let pb = popcount(b);
        let denom = pa.max(pb);
        if denom == 0 { 0 } else { (inter * 1000) / denom }  // scaled by 1000
    };

    let mut assignments: Vec<usize> = vec![0; n_fns];

    // K-means style iteration (3 rounds is usually enough for this data)
    for _iter in 0..3 {
        // Assignment step: assign each function to closest centroid
        for (i, bv_opt) in fn_bvs.iter().enumerate() {
            if let Some(bv) = bv_opt {
                let best = centroids.iter().enumerate()
                    .map(|(c_idx, c_bv)| (c_idx, similarity(bv, c_bv)))
                    .max_by_key(|(_, s)| *s)
                    .map(|(c_idx, _)| c_idx)
                    .unwrap_or(0);
                assignments[i] = best;
            }
        }

        // Update step: new centroid = bitwise OR of all assigned members' dep sets
        // (union of all symbols needed by this cluster — the minimal superset PCH)
        let mut new_centroids: Vec<Vec<u64>> = vec![vec![0u64; words]; k];
        let mut cluster_sizes: Vec<usize> = vec![0; k];
        for (i, bv_opt) in fn_bvs.iter().enumerate() {
            if let Some(bv) = bv_opt {
                let c = assignments[i];
                for (j, w) in bv.iter().enumerate() {
                    new_centroids[c][j] |= w;
                }
                cluster_sizes[c] += 1;
            }
        }
        // Reset empty clusters by stealing from largest
        for c in 0..k {
            if cluster_sizes[c] == 0 {
                if let Some(largest) = (0..k).max_by_key(|&c2| cluster_sizes[c2]) {
                    new_centroids[c] = new_centroids[largest].clone();
                }
            }
        }
        centroids = new_centroids;
    }

    // Build output: group fn_keys by assignment
    let mut clusters: Vec<Vec<String>> = vec![Vec::new(); k];
    for (i, fn_key) in fn_keys.iter().enumerate() {
        clusters[assignments[i]].push((*fn_key).clone());
    }
    clusters.retain(|c| !c.is_empty());
    clusters
}

/// Extract lines from the .i file that belong to a given set of source files.
/// Used to build a minimal PCH for a cluster.
/// Returns the content as a String.
fn extract_lines_for_files(
    file_content: &str,
    source_files: &FxHashSet<String>,
    skip_fn_bodies: bool,
) -> String {
    let lines: Vec<&str> = file_content.lines().collect();
    let n = lines.len();
    let mut out = String::with_capacity(file_content.len() / 4);
    let mut current_file = String::new();
    let mut in_relevant_file = false;
    let mut brace_depth: i32 = 0;
    let mut in_fn_body = false;
    let mut prev_ends_paren = false;

    // Minimal brace counter (same logic as the main PCH generator)
    let count_net_braces = |s: &str| -> i32 {
        let mut depth = 0i32;
        let bytes = s.as_bytes();
        let len = bytes.len();
        let mut j = 0usize;
        while j < len {
            match bytes[j] {
                b'"' => {
                    j += 1;
                    while j < len {
                        if bytes[j] == b'\\' { j += 2; continue; }
                        if bytes[j] == b'"' { break; }
                        j += 1;
                    }
                }
                b'\'' => {
                    j += 1;
                    while j < len {
                        if bytes[j] == b'\\' { j += 2; continue; }
                        if bytes[j] == b'\'' { break; }
                        j += 1;
                    }
                }
                b'/' if j + 1 < len && bytes[j + 1] == b'*' => {
                    j += 2;
                    while j + 1 < len {
                        if bytes[j] == b'*' && bytes[j + 1] == b'/' { j += 2; break; }
                        j += 1;
                    }
                    continue;
                }
                b'{' => depth += 1,
                b'}' => depth -= 1,
                _ => {}
            }
            j += 1;
        }
        depth
    };

    for i in 0..n {
        let line = lines[i];
        let trimmed = line.trim();

        // Track #line directives
        if trimmed.starts_with('#') {
            let rest = trimmed.trim_start_matches('#').trim_start();
            if rest.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false) {
                let end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
                let after = rest[end..].trim();
                if after.starts_with('"') {
                    let fname = after.trim_start_matches('"');
                    if let Some(end_q) = fname.find('"') {
                        current_file = fname[..end_q].to_string();
                        in_relevant_file = source_files.contains(&current_file)
                            || source_files.iter().any(|f| current_file.ends_with(f.as_str()));
                    }
                }
                // Always emit line markers so the compiler tracks locations
                out.push_str(line);
                out.push('\n');
                continue;
            }
        }

        if !in_relevant_file {
            prev_ends_paren = false;
            continue;
        }

        if skip_fn_bodies {
            // Detect function body start: line ending with ){ or standalone {
            // preceded by a line ending with )
            let is_fn_open = (trimmed.ends_with(") {") || trimmed.ends_with("){")
                || (trimmed == "{" && prev_ends_paren))
                && !in_fn_body;
            if is_fn_open && brace_depth == 0 {
                in_fn_body = true;
                // Emit a semicolon placeholder so forward declarations work
                out.push_str("/* fn body omitted for PCH */\n");
                brace_depth = count_net_braces(line);
                prev_ends_paren = false;
                continue;
            }
            if in_fn_body {
                brace_depth += count_net_braces(line);
                if brace_depth <= 0 {
                    in_fn_body = false;
                    brace_depth = 0;
                }
                prev_ends_paren = false;
                continue;
            }
        }

        out.push_str(line);
        out.push('\n');

        let is_paren_end = trimmed.ends_with(')')
            || trimmed.ends_with("__attribute__((noinline))")
            || trimmed.ends_with("__attribute__((cold))");
        prev_ends_paren = is_paren_end && !trimmed.is_empty() && !trimmed.starts_with('#');
    }

    out
}

/// Parse the .i file to build a map from source file path → (start_line, end_line) ranges.
/// Each entry covers the contiguous block of lines attributed to that file by #line markers.
/// Returns Vec<(file_path, start_byte_offset, end_byte_offset)> in file order.
fn parse_i_file_sections(file_content: &str) -> Vec<(String, usize, usize)> {
    let mut sections: Vec<(String, usize, usize)> = Vec::new();
    let mut current_file = String::new();
    let mut section_start = 0usize;
    let mut byte_offset = 0usize;

    for line in file_content.lines() {
        let line_len = line.len() + 1; // +1 for newline
        let trimmed = line.trim();

        // Parse #line markers: `# N "file"` or `# N "file" flags`
        if trimmed.starts_with('#') {
            let rest = trimmed.trim_start_matches('#').trim_start();
            if rest.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false) {
                let end_num = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
                let after = rest[end_num..].trim();
                if after.starts_with('"') {
                    let fname = after.trim_start_matches('"');
                    if let Some(end_q) = fname.find('"') {
                        let new_file = fname[..end_q].to_string();
                        if new_file != current_file {
                            // Save the completed section for the old file
                            if !current_file.is_empty() && byte_offset > section_start {
                                sections.push((current_file.clone(), section_start, byte_offset));
                            }
                            current_file = new_file;
                            section_start = byte_offset;
                        }
                    }
                }
            }
        }

        byte_offset += line_len;
    }

    // Push final section
    if !current_file.is_empty() && byte_offset > section_start {
        sections.push((current_file, section_start, byte_offset));
    }

    // Merge consecutive entries for the same file
    let mut merged: Vec<(String, usize, usize)> = Vec::new();
    for (file, start, end) in sections {
        if let Some(last) = merged.last_mut() {
            if last.0 == file && last.2 == start {
                last.2 = end;
                continue;
            }
        }
        merged.push((file, start, end));
    }
    merged
}

/// Build a reverse map: type_identifier → section_index for the section that *defines* it.
/// Only maps identifiers that are defined as types (typedef names, struct names, enum names)
/// in header sections. This avoids the cascading over-inclusion from generic identifiers.
fn build_typedef_to_section(
    sections: &[(String, usize, usize)],
    file_content: &str,
) -> FxHashMap<String, usize> {
    let mut typedef_to_sec: FxHashMap<String, usize> = FxHashMap::default();

    for (sec_idx, (file, start, end)) in sections.iter().enumerate() {
        // Only index header sections (.h files)
        if !file.ends_with(".h") && !file.ends_with(".hpp") {
            continue;
        }
        let end_byte = (*end).min(file_content.len());
        if *start >= end_byte { continue; }
        let sec_content = &file_content[*start..end_byte];

        for line in sec_content.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') { continue; }

            // Pattern 1a: `typedef ... name;` on a single line — last word before `;`
            if trimmed.starts_with("typedef ") || trimmed.contains(" typedef ") {
                let clean = trimmed.trim_end_matches(';').trim();
                if let Some(last_word) = clean.split_whitespace().last() {
                    let name = last_word.trim_start_matches('*').trim_matches(|c: char| !c.is_alphanumeric() && c != '_');
                    if name.len() > 2 && name.chars().all(|c| c.is_alphanumeric() || c == '_') {
                        typedef_to_sec.entry(name.to_string()).or_insert(sec_idx);
                    }
                }
            }
            // Pattern 1b: `} name;` or `} *name;` — closing line of a multi-line typedef struct/union
            // e.g. `} raw_spinlock_t;` from `typedef struct raw_spinlock { ... } raw_spinlock_t;`
            if trimmed.starts_with('}') && trimmed.ends_with(';') {
                let inner = trimmed.trim_start_matches('}').trim_end_matches(';').trim();
                // Remove any attributes like `__attribute__((...))`
                let clean = if let Some(pos) = inner.find("__attribute__") { &inner[..pos] } else { inner };
                let name = clean.trim().trim_start_matches('*').trim();
                if name.len() > 2 && name.chars().all(|c| c.is_alphanumeric() || c == '_') {
                    typedef_to_sec.insert(name.to_string(), sec_idx); // overwrite: definition wins
                }
            }

            // Pattern 2: `struct name {` or `union name {` or `enum name {`
            // Prefer full definitions (with `{`) over forward declarations.
            for kw in &["struct ", "union ", "enum "] {
                if let Some(rest) = trimmed.strip_prefix(kw) {
                    let name = rest.split(|c: char| !c.is_alphanumeric() && c != '_').next().unwrap_or("");
                    if name.len() > 1 {
                        let is_definition = trimmed.contains('{');
                        if is_definition {
                            // Full definition: always overwrite (definition > forward decl)
                            typedef_to_sec.insert(name.to_string(), sec_idx);
                            let kw_trim = kw.trim();
                            typedef_to_sec.insert(format!("{kw_trim}_{name}"), sec_idx);
                        } else {
                            // Forward declaration: only set if not already present
                            typedef_to_sec.entry(name.to_string()).or_insert(sec_idx);
                            let kw_trim = kw.trim();
                            typedef_to_sec.entry(format!("{kw_trim}_{name}")).or_insert(sec_idx);
                        }
                    }
                }
            }

            // Pattern 3: `DECLARE_xxx(name, ...)` kernel macros
            if trimmed.starts_with("DECLARE_") || trimmed.starts_with("DEFINE_") {
                if let Some(paren) = trimmed.find('(') {
                    if let Some(end_paren) = trimmed[paren+1..].find(|c: char| !c.is_alphanumeric() && c != '_') {
                        let name = &trimmed[paren+1..paren+1+end_paren];
                        if name.len() > 1 {
                            typedef_to_sec.entry(name.to_string()).or_insert(sec_idx);
                        }
                    }
                }
            }
        }
    }

    typedef_to_sec
}

/// For a set of function bodies, find header sections needed by:
///   1. Using the transitive dep graph to find which header files define used types
///   2. Scanning function body tokens against the typedef→section map as a fallback
/// Returns a BitSet of section indices (as Vec<u64>) that must be included in the PCH.
fn find_needed_sections(
    fn_keys: &[&String],
    fn_bodies: &[&str],
    typedef_to_sec: &FxHashMap<String, usize>,
    transitive_deps: &TransitiveDeps,
    sections: &[(String, usize, usize)],
    n_sections: usize,
) -> Vec<u64> {
    let words = (n_sections + 63) / 64;
    let mut needed = vec![0u64; words];

    // Build a file→section_index map for fast lookup by dep file path
    let mut file_to_sec: FxHashMap<&str, Vec<usize>> = FxHashMap::default();
    for (sec_idx, (file, _, _)) in sections.iter().enumerate() {
        file_to_sec.entry(file.as_str()).or_default().push(sec_idx);
    }

    // Step 1: Use dep graph — for each function, find which header files its deps come from
    for fn_key in fn_keys {
        if let Some(deps) = transitive_deps.deps.get(*fn_key) {
            for dep_key in deps {
                if let Some(dep_file) = extract_key_file(dep_key) {
                    if dep_file.ends_with(".h") || dep_file.ends_with(".hpp") {
                        if let Some(sec_indices) = file_to_sec.get(dep_file) {
                            for &sec_idx in sec_indices {
                                let word = sec_idx / 64;
                                let bit = sec_idx % 64;
                                needed[word] |= 1u64 << bit;
                            }
                        }
                    }
                }
            }
        }
    }

    // Step 2: Scan function body tokens against typedef→section map
    // This catches types not in the dep graph (e.g., via macros)
    for body in fn_bodies {
        let bytes = body.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'_' || bytes[i].is_ascii_alphabetic() {
                let start_i = i;
                while i < bytes.len() && (bytes[i] == b'_' || bytes[i].is_ascii_alphanumeric()) {
                    i += 1;
                }
                let ident = std::str::from_utf8(&bytes[start_i..i]).unwrap_or("");
                // Only look up type-like identifiers: _t suffix, CamelCase, or __prefix
                if ident.len() > 2 {
                    if let Some(&sec_idx) = typedef_to_sec.get(ident) {
                        let word = sec_idx / 64;
                        let bit = sec_idx % 64;
                        needed[word] |= 1u64 << bit;
                    }
                }
            } else {
                i += 1;
            }
        }
    }

    needed
}

/// Extract PCH content by assembling needed sections in file order.
/// Strips all `#line` / `# N "file"` markers to produce a flat header
/// that GCC can compile without the original include-nesting context.
/// Includes ALL header sections up to the highest-needed section index,
/// ensuring cascading type dependencies are always satisfied (preprocessed
/// files have sections in topological order).
fn assemble_pch_from_sections(
    sections: &[(String, usize, usize)],
    needed_bits: &[u64],
    file_content: &str,
    _include_c_section: bool,  // unused: we never include .c sections in PCH
) -> String {
    let mut out = String::new();

    for (sec_idx, (file, start, end)) in sections.iter().enumerate() {
        let word = sec_idx / 64;
        let bit = sec_idx % 64;
        let is_needed = word < needed_bits.len() && (needed_bits[word] >> bit) & 1 == 1;
        // Only include header sections that are needed
        let is_header = file.ends_with(".h") || file.ends_with(".hpp");
        if !is_needed || !is_header { continue; }

        let end_byte = (*end).min(file_content.len());
        if *start >= end_byte { continue; }

        let section_text = &file_content[*start..end_byte];

        // Emit section content, stripping only `#line` / `# N "file"` markers
        // to avoid GCC "file changed" nesting errors. Include all declarations
        // and inline bodies as-is — stripping inline bodies is unreliable for
        // complex kernel macro-expanded code.
        for line in section_text.lines() {
            let trimmed = line.trim();
            // Skip preprocessor linemarkers: `# N "file" flags`
            if trimmed.starts_with('#') {
                let rest = trimmed.trim_start_matches('#').trim_start();
                if rest.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false) {
                    continue; // linemarker — skip
                }
            }
            out.push_str(line);
            out.push('\n');
        }

        if !out.ends_with('\n') { out.push('\n'); }
    }

    out
}

/// Expand a set of header sections by iteratively adding sections that define
/// types referenced in the already-included sections.
/// `max_rounds`: number of expansion rounds (typically 2-3 is enough).
fn expand_sections_transitively(
    initial_bits: &[u64],
    sections: &[(String, usize, usize)],
    file_content: &str,
    typedef_to_sec: &FxHashMap<String, usize>,
    max_rounds: usize,
) -> Vec<u64> {
    let n_sections = sections.len();
    let words = (n_sections + 63) / 64;
    let mut bits = initial_bits.to_vec();
    if bits.len() < words { bits.resize(words, 0); }

    for _round in 0..max_rounds {
        let prev_count: u32 = bits.iter().map(|w| w.count_ones()).sum();

        // Collect content of currently included sections
        let mut included_content = String::new();
        for (sec_idx, (file, start, end)) in sections.iter().enumerate() {
            let word = sec_idx / 64;
            let bit = sec_idx % 64;
            if word >= bits.len() || (bits[word] >> bit) & 1 == 0 { continue; }
            if !file.ends_with(".h") && !file.ends_with(".hpp") { continue; }
            let end_byte = (*end).min(file_content.len());
            if *start < end_byte {
                included_content.push_str(&file_content[*start..end_byte]);
            }
        }

        // Tokenize and find any type name that needs an additional section
        let bytes = included_content.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'_' || bytes[i].is_ascii_alphabetic() {
                let start_i = i;
                while i < bytes.len() && (bytes[i] == b'_' || bytes[i].is_ascii_alphanumeric()) {
                    i += 1;
                }
                let ident = std::str::from_utf8(&bytes[start_i..i]).unwrap_or("");
                if ident.len() > 2 {
                    if let Some(&sec_idx) = typedef_to_sec.get(ident) {
                        let word = sec_idx / 64;
                        let bit = sec_idx % 64;
                        if word < bits.len() {
                            bits[word] |= 1u64 << bit;
                        }
                    }
                }
            } else {
                i += 1;
            }
        }

        let new_count: u32 = bits.iter().map(|w| w.count_ones()).sum();
        if new_count == prev_count { break; } // converged
    }

    bits
}

/// Cluster-PCH mode: group functions by header deps, write per-cluster minimal PCH + bundle.
/// Returns (cluster_count, total_fns).
fn cluster_pch_mode(
    filename: &str,
    pu_order: &[String],
    pu: &FxHashMap<String, String>,
    uids: &FxHashMap<String, usize>,
    preamble_fn_names: &FxHashSet<String>,
    transitive_deps: &TransitiveDeps,
    config: &SplitConfig,
) -> io::Result<(usize, usize)> {
    let n_clusters: usize = std::env::var("PRECC_CLUSTER_PCH_N")
        .ok().and_then(|s| s.parse().ok()).unwrap_or(8);

    // Collect function PUs to process (same filter as standard PCH mode)
    let fn_keys: Vec<&String> = pu_order.iter()
        .filter(|u| {
            if !uids.contains_key(*u) { return false; }
            if !config.should_process_uid(*uids.get(*u).unwrap()) { return false; }
            if let Some(fname) = extract_key_name(u) {
                if preamble_fn_names.contains(fname) { return false; }
            }
            // Only actual function/method PUs (not typedefs, structs, etc.)
            u.starts_with("function:")
        })
        .collect();

    eprintln!("cluster-PCH: clustering {} functions into up to {} clusters", fn_keys.len(), n_clusters);

    if fn_keys.is_empty() {
        eprintln!("cluster-PCH: no functions to cluster");
        return Ok((0, 0));
    }

    // Read full .i file content once
    let file_content = std::fs::read_to_string(filename)?;

    // Parse the .i file into sections (one per #line-delimited source file)
    let sections = parse_i_file_sections(&file_content);
    eprintln!("cluster-PCH: {} file sections in .i file", sections.len());

    // Build typedef→section map for precise header section lookup
    let typedef_to_sec = build_typedef_to_section(&sections, &file_content);
    eprintln!("cluster-PCH: {} type definitions indexed from headers", typedef_to_sec.len());

    // Compute per-function section bitsets — these are the "real" feature vectors for clustering.
    // Two functions with similar section bitsets need similar PCH content.
    let fn_section_bvs: Vec<Vec<u64>> = fn_keys.iter()
        .map(|k| {
            let body = pu.get(*k).map(|s| s.as_str()).into_iter().collect::<Vec<_>>();
            find_needed_sections(
                &[k],
                &body,
                &typedef_to_sec,
                transitive_deps,
                &sections,
                sections.len(),
            )
        })
        .collect();

    // Re-cluster using section bitsets instead of dep bitvectors
    // This gives us clusters that truly share the same PCH content
    let clusters = if fn_section_bvs.iter().all(|bv| bv.iter().all(|&w| w == 0)) {
        // No section info — fall back to dep-based clustering
        cluster_functions_by_headers(&fn_keys, transitive_deps, n_clusters)
    } else {
        // K-means on section bitsets
        let n_fns = fn_keys.len();
        let words = fn_section_bvs.first().map(|v| v.len()).unwrap_or(1);
        let k = n_clusters.min(n_fns);

        let popcount = |bv: &[u64]| -> u64 {
            bv.iter().map(|w| w.count_ones() as u64).sum()
        };
        let similarity = |a: &[u64], b: &[u64]| -> u64 {
            let inter: u64 = a.iter().zip(b.iter()).map(|(x, y)| (x & y).count_ones() as u64).sum();
            let pa = popcount(a);
            let pb = popcount(b);
            let denom = pa.max(pb);
            if denom == 0 { 0 } else { (inter * 1000) / denom }
        };

        let mut centroids: Vec<Vec<u64>> = (0..k)
            .map(|i| fn_section_bvs[(i * n_fns) / k].clone())
            .collect();

        let mut assignments: Vec<usize> = vec![0; n_fns];
        for _iter in 0..4 {
            for (i, bv) in fn_section_bvs.iter().enumerate() {
                let best = centroids.iter().enumerate()
                    .map(|(c, cbv)| (c, similarity(bv, cbv)))
                    .max_by_key(|(_, s)| *s)
                    .map(|(c, _)| c)
                    .unwrap_or(0);
                assignments[i] = best;
            }
            let mut new_centroids: Vec<Vec<u64>> = vec![vec![0u64; words]; k];
            let mut sizes: Vec<usize> = vec![0; k];
            for (i, bv) in fn_section_bvs.iter().enumerate() {
                let c = assignments[i];
                for (j, w) in bv.iter().enumerate() {
                    new_centroids[c][j] |= w;
                }
                sizes[c] += 1;
            }
            for c in 0..k {
                if sizes[c] == 0 {
                    if let Some(largest) = (0..k).max_by_key(|&c2| sizes[c2]) {
                        new_centroids[c] = new_centroids[largest].clone();
                    }
                }
            }
            centroids = new_centroids;
        }

        let mut result: Vec<Vec<String>> = vec![Vec::new(); k];
        for (i, fn_key) in fn_keys.iter().enumerate() {
            result[assignments[i]].push((*fn_key).clone());
        }
        result.retain(|c| !c.is_empty());
        result
    };

    eprintln!("cluster-PCH: {} non-empty clusters", clusters.len());

    let base_name = std::path::Path::new(filename)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| filename.to_string());

    // Use the standard PCH generated by the PCH mode (already written to filename.pch.h).
    // This avoids the cascading dependency issue of minimal section assembly —
    // the standard PCH is guaranteed correct. Each cluster bundle just includes it.
    let standard_pch_path = format!("{}.pch.h", filename);

    // Build a set of function names already defined (with bodies) in the PCH.
    // We scan the PCH content for identifier-before-'(' patterns to detect definitions.
    // Any function in pu that is also defined in the PCH must be excluded from bundles
    // to avoid redefinition errors.
    let pch_defined_fns: FxHashSet<String> = {
        let mut fns = FxHashSet::default();
        if let Ok(pch_content) = std::fs::read_to_string(&standard_pch_path) {
            // We detect function definitions: look for lines containing `identifier(`
            // followed (within a few lines) by `{`. We use a simple state machine:
            // a function is "defined" if we see its body (the PCH may contain bodies of
            // both inline and non-inline functions from headers).
            // Simple heuristic: find all lines matching "identifier(" at col-0 or as part
            // of a multi-line sig, followed by a line with just `{`.
            let id_before_paren = regex::Regex::new(r"([a-zA-Z_][a-zA-Z0-9_]*)\s*\(").unwrap();
            let pch_lines: Vec<&str> = pch_content.lines().collect();
            let npl = pch_lines.len();
            let mut pi = 0usize;
            while pi < npl {
                let pl = pch_lines[pi];
                let plt = pl.trim();
                // Skip linemarkers, preprocessor, comments
                if plt.starts_with('#') || plt.starts_with("//") || plt.starts_with("/*") {
                    pi += 1;
                    continue;
                }
                // Look for col-0 line that could be a function signature start
                let is_col0 = !pl.starts_with('\t') && !pl.starts_with("   ");
                // Allow struct/union/enum lines that are function return types (contain '*' before '(')
                // e.g. "struct foo *func_name(...)" is a function def, not a struct declaration.
                let is_struct_fn_ret = (plt.starts_with("struct ") || plt.starts_with("union "))
                    && plt.contains('(') && {
                        // Check if '(' is preceded by '*' (or identifier) — function returning pointer
                        let paren_pos = plt.find('(').unwrap_or(0);
                        let before_paren = plt[..paren_pos].trim_end();
                        before_paren.ends_with('*') || before_paren.chars().last().map(|c| c.is_alphanumeric() || c == '_').unwrap_or(false)
                    };
                if is_col0 && plt.contains('(') && !plt.starts_with("typedef")
                    && (is_struct_fn_ret || (!plt.starts_with("struct") && !plt.starts_with("union")))
                    && !plt.starts_with("enum") {
                    // Scan ahead up to 8 lines for a '{' that opens the function body
                    // (Style A: alone on a line) or a brace-balanced body on the sig line.
                    // First, check if the body is on this line (Style D/E).
                    let brace_net: i32 = plt.bytes().fold(0i32, |d, b| match b {
                        b'{' => d + 1, b'}' => d - 1, _ => d,
                    });
                    // Helper: extract function name from a line — the last identifier before '('
                    // that is not a C keyword or common attribute keyword.
                    let skip_ids: &[&str] = &["if", "while", "for", "switch", "do", "return",
                        "static", "inline", "extern", "const", "volatile", "void", "int",
                        "char", "long", "unsigned", "signed", "struct", "union", "enum",
                        "__attribute__", "__attribute", "__inline", "__inline__",
                        "__forceinline", "__always_inline"];
                    let extract_fn_name = |line_text: &str| -> Option<String> {
                        // Strip __attribute__((...)) blocks so their contents don't confuse name extraction
                        let stripped = {
                            let mut s = String::with_capacity(line_text.len());
                            let bytes = line_text.as_bytes();
                            let n = bytes.len();
                            let mut i2 = 0usize;
                            while i2 < n {
                                // Look for __attribute__( pattern
                                if i2 + 13 <= n && &bytes[i2..i2+13] == b"__attribute__" {
                                    // Skip to the matching close paren
                                    let mut depth = 0i32;
                                    let mut j = i2 + 13;
                                    while j < n {
                                        match bytes[j] {
                                            b'(' => { depth += 1; j += 1; }
                                            b')' => { depth -= 1; j += 1; if depth <= 0 { break; } }
                                            _ => { j += 1; }
                                        }
                                    }
                                    i2 = j;
                                } else {
                                    s.push(bytes[i2] as char);
                                    i2 += 1;
                                }
                            }
                            s
                        };
                        // Find the first qualifying identifier before '(' (that's the function name)
                        id_before_paren.captures_iter(&stripped)
                            .find(|c| { let s: &str = &c[1]; !skip_ids.contains(&s) })
                            .map(|c| c[1].to_string())
                    };
                    // Helper: try to extract fn name from a range of lines pi..pi+look_ahead
                    let extract_fn_name_multi = |start: usize, look_ahead: usize| -> Option<String> {
                        for li in start..npl.min(start + look_ahead) {
                            if let Some(fname) = extract_fn_name(pch_lines[li].trim()) {
                                return Some(fname);
                            }
                        }
                        None
                    };
                    if brace_net == 0 && plt.contains('{') {
                        // Complete body on this line — extract function name
                        if let Some(fname) = extract_fn_name_multi(pi, 3) {
                            fns.insert(fname);
                        }
                    } else if brace_net > 0 || (brace_net == 0 && !plt.contains('{')) {
                        // Multi-line: scan ahead for body open
                        let mut found_body = false;
                        let mut scan_j = pi + 1;
                        let paren_net: i32 = plt.bytes().fold(0i32, |d, b| match b {
                            b'(' => d + 1, b')' => d - 1, _ => d,
                        });
                        let mut running_paren = paren_net;
                        while scan_j < npl.min(pi + 12) {
                            let sl = pch_lines[scan_j].trim();
                            running_paren += sl.bytes().fold(0i32, |d, b| match b {
                                b'(' => d + 1, b')' => d - 1, _ => d,
                            });
                            if running_paren <= 0 {
                                if sl.contains('{') {
                                    found_body = true;
                                } else if scan_j + 1 < npl {
                                    let next_sl = pch_lines[scan_j + 1].trim();
                                    if next_sl.starts_with('{') || next_sl == ")" {
                                        // Look one more line if needed
                                        found_body = next_sl.starts_with('{') || {
                                            scan_j + 2 < npl && pch_lines[scan_j + 2].trim().starts_with('{')
                                        };
                                    }
                                }
                                break;
                            }
                            scan_j += 1;
                        }
                        if found_body {
                            // Try pi first, then next few lines (fn name may be on a continuation line)
                            if let Some(fname) = extract_fn_name_multi(pi, 4) {
                                fns.insert(fname);
                            }
                        }
                    }
                }
                pi += 1;
            }
        }
        eprintln!("cluster-PCH: {} fns defined in standard PCH (will be excluded from bundles)", fns.len());
        fns
    };

    let mut total_fns = 0usize;

    // Remove stale bundle files from previous runs with different cluster counts.
    if let Ok(entries) = std::fs::read_dir(".") {
        let prefix = format!("{}.cluster_", base_name);
        let suffix = ".bundle.pu.c";
        for entry in entries.flatten() {
            let name = entry.file_name().into_string().unwrap_or_default();
            if name.starts_with(&prefix) && name.ends_with(suffix) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }

    for (cluster_id, cluster_fns) in clusters.iter().enumerate() {
        // Collect function bodies and keys for this cluster
        let cluster_fn_refs: Vec<&String> = cluster_fns.iter()
            .filter_map(|k| fn_keys.iter().find(|&&fk| fk == k).copied())
            .collect();
        let fn_bodies: Vec<&str> = cluster_fns.iter()
            .filter_map(|k| pu.get(k.as_str()).map(|s| s.as_str()))
            .collect();

        // Find which header sections this cluster's functions actually use
        // Compute needed sections for this cluster (for stats)
        let needed_bits = find_needed_sections(
            &cluster_fn_refs,
            &fn_bodies,
            &typedef_to_sec,
            transitive_deps,
            &sections,
            sections.len(),
        );
        let final_bits = expand_sections_transitively(
            &needed_bits, &sections, &file_content, &typedef_to_sec, 3
        );
        let n_needed = needed_bits.iter().map(|w| w.count_ones()).sum::<u32>();
        let n_final = final_bits.iter().map(|w| w.count_ones()).sum::<u32>();

        // Write bundle file: include the standard PCH (guaranteed correct) + function bodies.
        // Using the shared PCH avoids cascading dependency issues of minimal section assembly.
        let bundle_name = format!("{}.cluster_{}.bundle.pu.c", base_name, cluster_id);
        let mut bundle_content = format!("#include \"{}\"\n\n", standard_pch_path);
        let mut skipped_fns = 0usize;
        for fn_key in cluster_fns {
            // Skip functions already defined in the PCH to avoid redefinition errors
            let fn_name = extract_key_name(fn_key).unwrap_or(fn_key.as_str());
            if pch_defined_fns.contains(fn_name) || preamble_fn_names.contains(fn_name) {
                skipped_fns += 1;
                continue;
            }
            if let Some(body) = pu.get(fn_key.as_str()) {
                // Skip PUs whose body starts with whitespace — these are ctags false-positives
                // where a function pointer parameter was misidentified as a function definition.
                let body_trim = body.trim_start_matches('\n');
                if body_trim.starts_with(' ') || body_trim.starts_with('\t') {
                    skipped_fns += 1;
                    continue;
                }
                // Skip PUs whose body starts with a bare function name (no return type).
                // These occur when the return type is on a separate line from the function name.
                // Emitting them causes "conflicting types" since the PCH declaration has the
                // correct return type while the bundle body implies implicit 'int'.
                // Detection: first line matches `identifier(` with no type prefix.
                {
                    let first_line = body_trim.lines().next().unwrap_or("").trim();
                    let type_prefixes = ["static", "extern", "inline", "void", "int", "char",
                        "long", "short", "unsigned", "signed", "const", "struct", "union",
                        "enum", "__attribute__", "_Bool", "bool", "float", "double",
                        "ssize_t", "size_t", "loff_t", "u8", "u16", "u32", "u64",
                        "s8", "s16", "s32", "s64", "uint", "ulong", "uint8_t", "uint16_t",
                        "uint32_t", "uint64_t", "int8_t", "int16_t", "int32_t", "int64_t",
                        "ptrdiff_t", "intptr_t", "uintptr_t", "gfp_t", "pgoff_t",
                        "irqreturn_t", "blk_status_t", "noinline", "__always_inline"];
                    let has_type_prefix = type_prefixes.iter().any(|p| {
                        first_line.starts_with(p) && (
                            first_line.len() == p.len() ||
                            first_line.as_bytes().get(p.len()).map(|&b| b == b' ' || b == b'\t' || b == b'*').unwrap_or(false)
                        )
                    });
                    // If no type prefix and line matches `identifier(`, this is missing return type
                    let looks_bare = !has_type_prefix && {
                        let bytes = first_line.as_bytes();
                        let mut i = 0;
                        while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') { i += 1; }
                        i > 0 && i < bytes.len() && (bytes[i] == b'(' || bytes[i] == b' ')
                    };
                    if looks_bare && first_line.contains('(') {
                        skipped_fns += 1;
                        continue;
                    }
                }
                // If the body is a packed SYSCALL_DEFINE line (brace-balanced, ends with ')')
                // the last segment is a truncated inline function declaration without ';'.
                // Append ';' so GCC sees a complete declaration, not a function definition start.
                let body_trimmed = body.trim_end();
                let brace_net: i32 = body_trimmed.bytes().fold(0i32, |d, b| match b {
                    b'{' => d + 1, b'}' => d - 1, _ => d,
                });
                if brace_net == 0 && body_trimmed.ends_with(')') && body_trimmed.contains('{') {
                    bundle_content.push_str(body_trimmed);
                    bundle_content.push_str(";\n");
                } else {
                    bundle_content.push_str(body);
                    bundle_content.push('\n');
                }
            }
            total_fns += 1;
        }
        std::fs::write(&bundle_name, &bundle_content)?;

        eprintln!("cluster-PCH: cluster {} — {}/{} fns (skipped {} in PCH), {}->{}/{} sections, bundle {} bytes",
            cluster_id, cluster_fns.len() - skipped_fns, cluster_fns.len(),
            skipped_fns, n_needed, n_final, sections.len(),
            bundle_content.len());
    }

    Ok((clusters.len(), total_fns))
}

pub fn main_wrapper(filename: &str) -> io::Result<()> {
    use std::time::Instant;
    let start_total = Instant::now();

    // Enable profiling if env var is set
    if std::env::var("RUST_PROFILE").is_ok() {
        enable_profiling();
    }

    // Initialize cached env vars
    init_debug_tags();

    // Load per-project config once at function entry.  If `.precc-config.toml` exists
    // in the same directory as the .i file, use it to skip per-file scanning.
    // Stored as owned clone so it's available throughout the entire function body.
    let cfg_entry: Option<PreccFileEntry> = load_config_for_file(filename)
        .and_then(|c| config_entry_for_file(&c, filename).cloned());

    // Fast basename check (O(1), no file I/O) for known-problematic files
    if is_problematic_basename(filename) {
        // scan_file_properties not needed for basename-matched files; is_incomplete=false is safe
        // since the reason for passthrough is the basename, not content.
        return passthrough_file(filename, false);
    }

    // PASSTHROUGH_THRESHOLD: brace-count dimension of the 2D split heuristic.
    // Default 50, calibrated on 558 kernel files (0% classification error vs per-file oracle).
    // The second dimension — src_frac >= 5% — fires independently regardless of this value.
    // Legacy: values >10000 treated as byte-size threshold (no src_frac check).
    // Set to 0 to always split; set to a large number to suppress brace-based splitting
    // (src_frac arm still active unless threshold >10000).
    let passthrough_threshold: u64 = std::env::var("PASSTHROUGH_THRESHOLD")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(50);

    if passthrough_threshold > 0 {
        let threshold_is_bytes = passthrough_threshold > 10_000;
        if threshold_is_bytes {
            // Legacy byte-size threshold — metadata only, no file scan needed
            let file_too_small = std::fs::metadata(filename)
                .map(|m| m.len() < passthrough_threshold)
                .unwrap_or(false);
            if file_too_small {
                let quiet_mode = std::env::var("QUIET").is_ok();
                if !quiet_mode {
                    eprintln!("Note: Using passthrough for {} (file size < {} bytes threshold)",
                        filename, passthrough_threshold);
                }
                return passthrough_file(filename, false);
            }
        } else {
            // Single-pass scan: brace count + src fraction + incomplete-file detection combined.
            // Split heuristic: split if braces >= threshold (enough functions to benefit from
            // per-function incremental compilation).  src_frac is not used as a gate here
            // because kernel files have a large header preamble that keeps src_frac < 5%
            // even for files with 100+ functions.  The unity build file (see below) eliminates
            // the fresh-build cost, so the only criterion is "enough PUs to be worth splitting."
            // PASSTHROUGH_THRESHOLD: brace threshold, default 50.
            // Config-first: use cached metrics if a fresh config entry exists,
            // otherwise fall back to scan_file_properties (original behaviour).
            let (brace_count, src_frac, is_incomplete) = if let Some(ref entry) = cfg_entry {
                let split_worthy = entry.fn_braces >= passthrough_threshold as usize;
                let cluster_pch_override = std::env::var("PRECC_CLUSTER_PCH").is_ok();
                if (!split_worthy || entry.strategy == BuildStrategy::Passthrough) && !cluster_pch_override {
                    let quiet_mode = std::env::var("QUIET").is_ok();
                    if !quiet_mode {
                        eprintln!("Note: Using passthrough for {} (config: strategy={}, braces={} src={:.1}%)",
                            filename, entry.strategy, entry.fn_braces, entry.src_frac * 100.0);
                    }
                    return passthrough_file(filename, false);
                }
                (entry.fn_braces, entry.src_frac, false)
            } else {
                scan_file_properties(filename)
            };
            let _ = src_frac; // available for diagnostics but not used as a split gate
            let split_worthy = brace_count >= passthrough_threshold as usize;
            let cluster_pch_override2 = std::env::var("PRECC_CLUSTER_PCH").is_ok();
            if (is_incomplete || !split_worthy) && !cluster_pch_override2 {
                let quiet_mode = std::env::var("QUIET").is_ok();
                if !quiet_mode {
                    if is_incomplete {
                        eprintln!("Note: Using passthrough for {} (incomplete preprocessed file)",
                            filename);
                    } else {
                        eprintln!("Note: Using passthrough for {} (braces={} src={:.1}% — below split threshold)",
                            filename, brace_count, src_frac * 100.0);
                    }
                }
                return passthrough_file(filename, is_incomplete);
            }
        }
    }

    // Reset the global state
    with_tag_info(|tag_info| *tag_info = TagInfo::default());

    // Extract glibc internal typedefs like __uint16_t, __uint32_t that are self-contained
    // (only use primitive types). The old extract_system_typedefs was too broad and caused issues.
    let system_typedefs = extract_glibc_internal_typedefs(filename);
    let _ = extract_system_typedefs; // silence unused warning for the old function

    // Store system typedefs in TAG_INFO
    with_tag_info(|tag_info| tag_info.system_typedefs = system_typedefs);

    // Extract extern function declarations that ctags doesn't capture
    // These are needed for system functions like close(), read(), write()
    let extern_functions = extract_extern_functions(filename);
    // Bug48 + Bug17: Extract extern variable declarations that ctags doesn't capture
    // These are needed for extern const struct declarations and extern arrays
    let extern_variables = extract_extern_variables(filename);
    if std::env::var("DEBUG_EXTERN").is_ok() && !extern_variables.is_empty() {
        eprintln!("DEBUG: Extracted extern variables:");
        for (name, decl) in &extern_variables {
            eprintln!("  {} -> {}", name, decl);
        }
    }
    // Bug71: Extract static function pointer variable declarations that ctags doesn't capture
    // Pattern: static <type> *((*name)(<params>));
    let static_funcptr_vars = extract_static_funcptr_vars(filename);
    with_tag_info(|tag_info| {
        tag_info.extern_functions = extern_functions;
        tag_info.extern_variables = extern_variables;
        tag_info.static_funcptr_vars = static_funcptr_vars;
    });

    // Initialize and process with dctags directly
    let start_ctags = Instant::now();
    let dctags = DCTags::new()
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;

    clear_input_buffer();
    with_tag_info(|tag_info| tag_info.postponed.clear());

    dctags.process_file_direct(filename)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;

    let ctags_time = start_ctags.elapsed();
    let quiet_mode = std::env::var("QUIET").is_ok();
    if !quiet_mode {
        eprintln!("[TIMING] ctags processing: {:.6}s", ctags_time.as_secs_f64());
    }

    // Process any remaining postponed entry
    // Extract data first, then release the lock before calling process_entry
    // Note: allow empty lines here because fill_bodies_from_line_numbers will fill
    // the body later using ctags line numbers
    let postponed_data: Option<(PuType, String, String, PuType, String)> = with_tag_info(|tag_info| {
        if let (Some(kind_str), Some(name_str), Some(file_str)) =
            (&tag_info.postponed.kind, &tag_info.postponed.name, &tag_info.postponed.file) {
            let scope_kind_str = tag_info.postponed.scope_kind.as_deref().unwrap_or("");
            let scope_name_str = tag_info.postponed.scope_name.as_deref().unwrap_or("");
            let pu_type = PuType::from_str(kind_str);
            // Only process function/variable/struct/union/typedef kinds that have a real name
            if !name_str.is_empty() && matches!(pu_type,
                PuType::Function | PuType::Variable | PuType::Struct |
                PuType::Union | PuType::Typedef | PuType::Enum | PuType::Prototype)
            {
                let scope_type = if scope_kind_str.is_empty() { PuType::Unknown } else { PuType::from_str(scope_kind_str) };
                return Some((pu_type, name_str.clone(), file_str.clone(), scope_type, scope_name_str.to_owned()));
            }
        }
        None
    });
    if let Some((pu_type, name, file, scope_type, scope_name)) = postponed_data {
        process_entry(pu_type, &name, &file, scope_type, &scope_name);
    }

    // Fill in function bodies using ctags line numbers (body-capture fallback)
    // This handles the case where the char-by-char buffer wasn't populated
    fill_bodies_from_line_numbers(filename);

    // Fallback: scan for function definitions ctags missed (e.g., when ctags aborted early
    // due to complex GCC constructs like _Generic with typeof)
    scan_uncovered_functions(filename);

    // Process the collected information
    let start_processing = Instant::now();
    let compute_dep_data = with_tag_info(|tag_info| {
        // Handle any remaining content
        if !tag_info.lines.is_empty() && !tag_info.pu_order.is_empty() {
            let key = tag_info.pu_order.last().unwrap().to_string();
            let lines = tag_info.lines.clone();
            if let Some(existing) = tag_info.pu.get_mut(&key) {
                existing.push_str(&lines);
            } else {
                tag_info.pu.insert(key.clone(), lines);
            }
        }

        // Process tags and collect results (sequential inside mutex to avoid Rayon deadlock)
        let tags_vec: Vec<(String, String)> = tag_info.pu_order.iter()
            .filter_map(|u| {
                let mut parts = u.splitn(3, ':');
                if let (Some(type_str), Some(name), Some(file_str)) = (parts.next(), parts.next(), parts.next()) {
                    Some((name.to_string(), format!("{}:{}", type_str, file_str)))
                } else {
                    None
                }
            })
            .collect();

        // Merge into tags map
        for (name, value) in tags_vec {
            tag_info.tags.entry(name).or_default().push(value);
        }

        // DEBUG: Print all pu_order entries
        if std::env::var("DEBUG_PU").is_ok() {
            eprintln!("=== pu_order entries === (count: {})", tag_info.pu_order.len());
            for (i, u) in tag_info.pu_order.iter().enumerate() {
                eprintln!("  [{}] '{}' (len={})", i, u, u.len());
            }
            if tag_info.pu_order.is_empty() {
                eprintln!("  (empty!)");
            }
            eprintln!("=== pu code contents ===");
            for u in tag_info.pu_order.iter() {
                if let Some(code) = tag_info.pu.get(u) {
                    eprintln!("  {} -> {} bytes: {:?}...", u, code.len(), &code.chars().take(100).collect::<String>());
                }
            }
            eprintln!("=== tags map ===");
            for (k, v) in tag_info.tags.iter() {
                eprintln!("  {} -> {:?}", k, v);
            }
            eprintln!("=== dependencies ===");
            for (k, v) in tag_info.dep.iter() {
                eprintln!("  {} depends on {:?}", k, v);
            }
        }

        // In split mode, only assign UIDs to functions (each function gets its own file)
        // In no-split mode, UIDs aren't used (uid=0 passed to use_dependency)
        let is_split_mode = std::env::var("SPLIT").is_ok();

        // SPLIT_COUNT: Number of output files to generate (chunked split mode)
        // If set, functions are grouped into N chunks instead of one file per function
        // E.g., SPLIT_COUNT=4 with 40 functions = 10 functions per file
        let split_count: Option<usize> = std::env::var("SPLIT_COUNT")
            .ok()
            .and_then(|s| s.parse().ok())
            .filter(|&n| n > 0);

        // Build UIDs and PIDs (sequential inside mutex to avoid Rayon deadlock)
        let (uids, pids): (Vec<_>, Vec<_>) = tag_info.pu_order.iter()
            .enumerate()
            .filter_map(|(idx, u)| {
                let type_str = u.split(':').next().unwrap_or("");
                let pu_type = PuType::from_str(type_str);
                let should_include = if is_split_mode {
                    // Split mode: Only functions get their own files
                    pu_type.is_function()
                } else {
                    // No-split mode: Track all types for dependency resolution
                    pu_type.is_nosplit_tracked()
                };

                if should_include {
                    Some((u.to_string(), idx))
                } else {
                    None
                }
            })
            .unzip();

        // Convert to hashmaps with calculated UIDs
        // In chunked mode, multiple functions share the same UID (same output file)
        let num_functions = uids.len();
        let mut uids_map = FxHashMap::with_capacity_and_hasher(num_functions, Default::default());
        let mut pids_map = FxHashMap::with_capacity_and_hasher(num_functions, Default::default());

        // Calculate chunk size for chunked split mode
        let (chunk_size, actual_chunks) = if let Some(target_chunks) = split_count {
            if target_chunks >= num_functions {
                // More chunks requested than functions, use 1:1 mapping
                (1, num_functions)
            } else {
                // Distribute functions evenly across chunks
                let size = (num_functions + target_chunks - 1) / target_chunks;
                (size, target_chunks)
            }
        } else {
            // Default: each function gets its own file (chunk_size = 1)
            (1, num_functions)
        };

        if split_count.is_some() && !quiet_mode {
            eprintln!("[SPLIT] Chunked mode: {} functions -> {} files ({} per file)",
                     num_functions, actual_chunks, chunk_size);
        }

        for (seq_idx, (key, idx)) in uids.into_iter().zip(pids.into_iter()).enumerate() {
            // In chunked mode, UID = (seq_idx / chunk_size) + 1
            // This groups consecutive functions into the same output file
            let uid = (seq_idx / chunk_size) + 1;
            uids_map.insert(key.clone(), uid);
            pids_map.insert(key, idx);
        }

        let processing_time = start_processing.elapsed();
        if !quiet_mode {
            eprintln!("[TIMING] tag processing: {:.6}s", processing_time.as_secs_f64());
        }

        // Clone data needed for compute_dependency so we can release the mutex before calling it
        // compute_dependency uses heavy Rayon parallelism which can deadlock if the mutex is held
        let pu_order_clone = tag_info.pu_order.clone();
        let dep_clone = tag_info.dep.clone();
        let tags_clone = tag_info.tags.clone();
        let pu_clone = tag_info.pu.clone();
        let enumerator_to_enum_clone = tag_info.enumerator_to_enum.clone();
        let system_typedefs_clone = tag_info.system_typedefs.clone();
        let extern_functions = tag_info.extern_functions.clone();
        let extern_variables = tag_info.extern_variables.clone();
        let static_funcptr_vars = tag_info.static_funcptr_vars.clone();
        (pu_order_clone, dep_clone, tags_clone, pu_clone, enumerator_to_enum_clone, system_typedefs_clone, extern_functions, extern_variables, static_funcptr_vars, uids_map, pids_map)
    });

    // compute_dependency uses heavy Rayon parallelism — call OUTSIDE the mutex to avoid deadlock
    let (pu_order_clone, dep_clone, tags_clone, pu_clone, enumerator_to_enum_clone, system_typedefs_clone, extern_functions, extern_variables, static_funcptr_vars, uids_map, pids_map) = compute_dep_data;
    let start_deps = Instant::now();
    compute_dependency(&pu_order_clone, uids_map, pids_map, &dep_clone, &tags_clone, &pu_clone, filename, &enumerator_to_enum_clone, &system_typedefs_clone, &extern_functions, &extern_variables, &static_funcptr_vars);
    let deps_time = start_deps.elapsed();
    if !quiet_mode {
        eprintln!("[TIMING] dependency computation: {:.6}s", deps_time.as_secs_f64());
    }

    let total_time = start_total.elapsed();
    if !quiet_mode {
        eprintln!("[TIMING] TOTAL: {:.6}s", total_time.as_secs_f64());
    }

    // Print Rust callback profiling stats if enabled
    print_profile_stats();

    Ok(())
}

/// Process multiple files sequentially in batch mode
/// For parallel processing, use process_files_parallel instead
pub fn process_files_batch(filenames: &[String]) -> io::Result<()> {
    use std::time::Instant;

    let start = Instant::now();
    let num_files = filenames.len();

    eprintln!("Processing {} files sequentially...", num_files);
    eprintln!("Tip: Use PARALLEL=1 for parallel processing");
    eprintln!();

    let mut successful = 0;
    let mut failed = 0;

    for (i, filename) in filenames.iter().enumerate() {
        eprintln!("[{}/{}] Processing: {}", i + 1, num_files, filename);
        let file_start = Instant::now();

        match main_wrapper(filename) {
            Ok(_) => {
                successful += 1;
                eprintln!("✓ Completed in {:.2}s\n", file_start.elapsed().as_secs_f64());
            }
            Err(e) => {
                failed += 1;
                eprintln!("✗ Error: {}\n", e);
            }
        }
    }

    let total_time = start.elapsed();
    eprintln!("Batch processing complete:");
    eprintln!("  Successful: {}", successful);
    eprintln!("  Failed: {}", failed);
    eprintln!("  Total time: {:.2}s", total_time.as_secs_f64());

    if failed > 0 {
        eprintln!("\nNote: {} file(s) failed processing", failed);
    }

    Ok(())
}

/// Process multiple files in parallel using subprocess spawning
/// Each file is processed in a separate process to avoid global state conflicts
/// Uses rayon's thread pool to manage parallelism
pub fn process_files_parallel(filenames: &[String]) -> io::Result<()> {
    use std::time::Instant;
    use std::process::Command;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let start = Instant::now();
    let num_files = filenames.len();
    let successful = AtomicUsize::new(0);
    let failed = AtomicUsize::new(0);
    let processed = AtomicUsize::new(0);

    // Get the path to the current executable
    let exe_path = std::env::current_exe()
        .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("Failed to get executable path: {}", e)))?;

    // Check for SPLIT environment variable
    let split_mode = std::env::var("SPLIT").is_ok();

    // Get number of parallel jobs from JOBS env var or use available parallelism
    let num_jobs: usize = std::env::var("JOBS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| std::thread::available_parallelism().map(|p| p.get()).unwrap_or(4));

    eprintln!("Processing {} files in parallel ({} jobs)...", num_files, num_jobs);
    eprintln!();

    // Configure rayon thread pool
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(num_jobs)
        .build()
        .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("Failed to create thread pool: {}", e)))?;

    pool.install(|| {
        filenames.par_iter().for_each(|filename| {
            let mut cmd = Command::new(&exe_path);
            cmd.arg(filename);

            if split_mode {
                cmd.env("SPLIT", "1");
            }

            // Suppress timing output in child processes
            cmd.env("QUIET", "1");

            match cmd.output() {
                Ok(output) => {
                    if output.status.success() {
                        successful.fetch_add(1, Ordering::Relaxed);
                    } else {
                        failed.fetch_add(1, Ordering::Relaxed);
                        eprintln!("✗ Error processing {}: {}", filename,
                            String::from_utf8_lossy(&output.stderr));
                    }
                }
                Err(e) => {
                    failed.fetch_add(1, Ordering::Relaxed);
                    eprintln!("✗ Failed to spawn process for {}: {}", filename, e);
                }
            }

            let done = processed.fetch_add(1, Ordering::Relaxed) + 1;
            if done % 10 == 0 || done == num_files {
                eprintln!("[{}/{}] files processed...", done, num_files);
            }
        });
    });

    let total_time = start.elapsed();
    let succ = successful.load(Ordering::Relaxed);
    let fail = failed.load(Ordering::Relaxed);

    eprintln!();
    eprintln!("Parallel processing complete:");
    eprintln!("  Successful: {}", succ);
    eprintln!("  Failed: {}", fail);
    eprintln!("  Total time: {:.2}s", total_time.as_secs_f64());
    eprintln!("  Throughput: {:.1} files/s", num_files as f64 / total_time.as_secs_f64());

    if fail > 0 {
        eprintln!("\nNote: {} file(s) failed processing", fail);
    }

    Ok(())
}

/// Process multiple files in parallel using in-process ctags.
///
/// This mode uses rayon for parallel processing with ctags running in-process.
/// Initialization and option parsing are protected by mutexes in ctags.
/// Per-file parsing state uses thread-local storage.
pub fn process_files_inprocess(filenames: &[String]) -> io::Result<()> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use rayon::prelude::*;

    let start = Instant::now();
    let num_files = filenames.len();
    let successful = AtomicUsize::new(0);
    let failed = AtomicUsize::new(0);
    let processed = AtomicUsize::new(0);

    let quiet_mode = std::env::var("QUIET").is_ok();
    let num_threads = rayon::current_num_threads();

    if !quiet_mode {
        eprintln!("Processing {} files in parallel using {} threads (in-process)...", num_files, num_threads);
        eprintln!();
    }

    // Process files in parallel - ctags uses mutexes for init and TLS for per-file state
    filenames.par_iter().for_each(|filename| {
        // Enable parallel mode for this thread's TagInfo
        set_parallel_mode(true);
        reset_thread_tag_info();

        // Process the file
        match main_wrapper(filename) {
            Ok(_) => {
                successful.fetch_add(1, Ordering::Relaxed);
            }
            Err(e) => {
                failed.fetch_add(1, Ordering::Relaxed);
                eprintln!("✗ Error processing {}: {}", filename, e);
            }
        }

        let count = processed.fetch_add(1, Ordering::Relaxed) + 1;
        if !quiet_mode && (count % 20 == 0 || count == num_files) {
            eprintln!("[{}/{}] files processed...", count, num_files);
        }
    });

    let total_time = start.elapsed();
    let succ = successful.load(Ordering::Relaxed);
    let fail = failed.load(Ordering::Relaxed);

    if !quiet_mode {
        eprintln!();
        eprintln!("In-process parallel processing complete:");
        eprintln!("  Successful: {}", succ);
        eprintln!("  Failed: {}", fail);
        eprintln!("  Total time: {:.2}s", total_time.as_secs_f64());
        eprintln!("  Throughput: {:.1} files/s", num_files as f64 / total_time.as_secs_f64());
    }

    if fail > 0 {
        eprintln!("\nNote: {} file(s) failed processing", fail);
    }

    Ok(())
}

/// Process multiple files with cross-file optimization for parallel builds.
///
/// This mode:
/// 1. Analyzes all files to identify shared types and cross-file dependencies
/// 2. Generates a project-wide common header with shared type declarations
/// 3. Processes all files in parallel, with PUs using the common header
/// 4. Outputs a Makefile for optimal parallel compilation based on dependency graph
pub fn process_files_crossfile_optimized(filenames: &[String], output_dir: Option<&std::path::Path>) -> io::Result<()> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use rayon::prelude::*;
    use std::path::Path;

    let start = Instant::now();
    let num_files = filenames.len();
    let quiet_mode = std::env::var("QUIET").is_ok();

    if !quiet_mode {
        eprintln!("=== Cross-File Optimized Build ===");
        eprintln!("Processing {} files with cross-file optimization...", num_files);
        eprintln!();
    }

    // Phase 1: Cross-file analysis
    if !quiet_mode {
        eprintln!("Phase 1: Analyzing cross-file dependencies...");
    }
    let analysis_start = Instant::now();
    let crossfile_deps = analyze_project_dependencies(filenames)?;
    let analysis_time = analysis_start.elapsed();

    if !quiet_mode {
        eprintln!("  Found {} common types across {} files",
            crossfile_deps.stats.common_type_count, num_files);
        eprintln!("  Cross-file function calls: {}", crossfile_deps.stats.cross_file_calls);
        eprintln!("  Analysis time: {:.2}s", analysis_time.as_secs_f64());
        eprintln!();
    }

    // Phase 2: Analyze common types (for informational purposes)
    // Note: Cross-file common header generation is disabled due to complex type ordering issues.
    // Each file uses its own per-file type resolution. The value of cross-file analysis is in:
    // - Dependency graph for build ordering
    // - Makefile generation for parallel compilation
    let output_base = output_dir.unwrap_or_else(|| Path::new("."));

    if !quiet_mode {
        eprintln!("Phase 2: Cross-file dependency analysis...");
        eprintln!("  Common types identified: {} (used for dependency tracking)",
            crossfile_deps.stats.common_type_count);
    }

    // Phase 3: Process files in parallel
    if !quiet_mode {
        eprintln!();
        eprintln!("Phase 3: Generating PU files in parallel...");
    }

    let gen_start = Instant::now();
    let successful = AtomicUsize::new(0);
    let failed = AtomicUsize::new(0);
    let processed = AtomicUsize::new(0);
    let total_pus = AtomicUsize::new(0);

    let num_threads = rayon::current_num_threads();

    filenames.par_iter().for_each(|filename| {
        set_parallel_mode(true);
        reset_thread_tag_info();

        match main_wrapper(filename) {
            Ok(_) => {
                successful.fetch_add(1, Ordering::Relaxed);
                // Count generated PU files (named <filename>_<uid>.pu.c)
                let file_path = Path::new(filename);
                let file_name = file_path.file_name().unwrap_or_default().to_string_lossy();
                let parent = file_path.parent();
                let dir = match parent {
                    Some(p) if !p.as_os_str().is_empty() => p.to_string_lossy().to_string(),
                    _ => ".".to_string(),
                };
                let pattern = format!("{}/{}_*.pu.c", dir, file_name);
                if let Ok(entries) = glob::glob(&pattern) {
                    total_pus.fetch_add(entries.count(), Ordering::Relaxed);
                }
            }
            Err(e) => {
                failed.fetch_add(1, Ordering::Relaxed);
                eprintln!("✗ Error processing {}: {}", filename, e);
            }
        }

        let count = processed.fetch_add(1, Ordering::Relaxed) + 1;
        if !quiet_mode && (count % 10 == 0 || count == num_files) {
            eprintln!("  [{}/{}] files processed...", count, num_files);
        }
    });

    let gen_time = gen_start.elapsed();
    let succ = successful.load(Ordering::Relaxed);
    let fail = failed.load(Ordering::Relaxed);
    let pus = total_pus.load(Ordering::Relaxed);

    // Phase 4: Generate parallel compilation Makefile
    if !quiet_mode {
        eprintln!();
        eprintln!("Phase 4: Generating parallel compilation Makefile...");
    }

    let makefile_path = output_base.join("Makefile.precc");
    generate_parallel_makefile(&makefile_path, filenames, &crossfile_deps)?;

    if !quiet_mode {
        eprintln!("  Generated: {}", makefile_path.display());
    }

    // Summary
    let total_time = start.elapsed();
    if !quiet_mode {
        eprintln!();
        eprintln!("=== Cross-File Optimization Complete ===");
        eprintln!("  Files processed: {}/{}", succ, num_files);
        eprintln!("  PU files generated: ~{}", pus);
        eprintln!("  Common types extracted: {}", crossfile_deps.stats.common_type_count);
        eprintln!("  Analysis time: {:.2}s", analysis_time.as_secs_f64());
        eprintln!("  Generation time: {:.2}s ({} threads)", gen_time.as_secs_f64(), num_threads);
        eprintln!("  Total time: {:.2}s", total_time.as_secs_f64());
        eprintln!();
        eprintln!("To compile in parallel, run:");
        eprintln!("  make -f {} -j$(nproc)", makefile_path.display());
    }

    if fail > 0 {
        eprintln!("\nNote: {} file(s) failed processing", fail);
    }

    Ok(())
}

/// Generate a Makefile for parallel compilation of PU files
fn generate_parallel_makefile(
    path: &std::path::Path,
    filenames: &[String],
    crossfile_deps: &crossfile::CrossFileDeps,
) -> io::Result<()> {
    use std::fs::File;
    use std::io::Write;
    use std::path::Path;

    let mut f = File::create(path)?;

    writeln!(f, "# Auto-generated Makefile for parallel PU compilation")?;
    writeln!(f, "# Generated by precc with cross-file optimization")?;
    writeln!(f, "#")?;
    writeln!(f, "# Usage: make -f {} -j$(nproc)", path.display())?;
    writeln!(f)?;

    writeln!(f, "CC ?= gcc")?;
    writeln!(f, "CFLAGS ?= -O2 -g")?;
    writeln!(f)?;

    // Collect all PU files per source file
    let mut all_objects = Vec::new();
    let mut source_to_pus: FxHashMap<String, Vec<String>> = FxHashMap::default();

    for filename in filenames {
        // PU files are named <filename>_<uid>.pu.c (e.g., arabic.i_1.pu.c)
        let file_path = Path::new(filename);
        let file_name = file_path.file_name().unwrap_or_default().to_string_lossy();
        let parent = file_path.parent();
        let dir = match parent {
            Some(p) if !p.as_os_str().is_empty() => p.to_string_lossy().to_string(),
            _ => ".".to_string(),
        };

        // Find PU files for this source
        let pattern = format!("{}/{}_*.pu.c", dir, file_name);
        if let Ok(entries) = glob::glob(&pattern) {
            let pus: Vec<String> = entries
                .filter_map(|e| e.ok())
                .map(|p| p.to_string_lossy().to_string())
                .collect();

            for pu in &pus {
                let obj = pu.replace(".pu.c", ".o");
                all_objects.push(obj.clone());
            }
            source_to_pus.insert(filename.clone(), pus);
        }
    }

    // Default target: all objects
    writeln!(f, "OBJECTS := {}", all_objects.join(" \\\n    "))?;
    writeln!(f)?;
    writeln!(f, ".PHONY: all clean")?;
    writeln!(f)?;
    writeln!(f, "all: $(OBJECTS)")?;
    writeln!(f)?;

    // Pattern rule for compiling PU files
    writeln!(f, "%.o: %.pu.c")?;
    writeln!(f, "\t$(CC) $(CFLAGS) -c $< -o $@")?;
    writeln!(f)?;

    // Add dependency rules based on cross-file analysis
    // Files that depend on others should compile after their dependencies
    if !crossfile_deps.file_dependencies.is_empty() {
        writeln!(f, "# Cross-file dependencies (for link ordering)")?;
        for (file, deps) in &crossfile_deps.file_dependencies {
            let file_stem = Path::new(file)
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy();

            for dep in deps {
                let dep_stem = Path::new(dep)
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy();

                writeln!(f, "# {} depends on {}", file_stem, dep_stem)?;
            }
        }
        writeln!(f)?;
    }

    // Clean target
    writeln!(f, "clean:")?;
    writeln!(f, "\trm -f $(OBJECTS)")?;

    Ok(())
}

fn process_enum_or_typedef(
    to_dep: &mut Vec<String>,
    dep: &mut FxHashMap<String,Vec<String>>,
    tags: &mut FxHashMap<String, Vec<String>>,
    pu: &FxHashMap<String, String>,
    pu_type: PuType,
    name: &str,
    file_str: &str,
    u: &str,
) {
    let type_str = pu_type.as_str();
    let cloned_dep = dep.clone();
    for to_dep_l in to_dep.iter() {
        if pu.get::<String>(&u.to_string()).unwrap_or(&String::new()).is_empty()
            && name != to_dep_l
        {
            if to_dep_l.contains(":") {
                if let Some(get) = dep.get_mut(to_dep_l) {
                    get.push(name.to_string());
                } else {
                    dep.insert(to_dep_l.to_string(), vec![name.to_string()]);
                }                
                let x: Vec<&str> = to_dep_l.split(":").collect();
                tags.entry(x[1].to_string())
                    .or_default()
                    .push(format!("{}:{}", x[0], x[2]));
            } else if let Some(enumerators) = cloned_dep.get(&format!("enumerator:{}:{}", to_dep_l, file_str)) {                
		for name in enumerators.iter() {
			if let Some(get) = dep.get_mut(u) {
			    get.push(to_dep_l.to_string());
			} else {
			    dep.insert(u.to_string(), vec![to_dep_l.to_string()]);
			}                
			    tags.entry(name.to_string())
				.or_default()
				.push(format!("{}:{}", type_str, file_str));
		}
            } else {
		if let Some(get) = dep.get_mut(u) {
		    get.push(to_dep_l.to_string());
		} else {
		    dep.insert(u.to_string(), vec![to_dep_l.to_string()]);
		}
                tags.entry(name.to_string())
                    .or_default()
                    .push(format!("{}:{}", type_str, file_str));
	    }
        } 
    }
}

fn process_externvar(
    lines: &mut String,
    headlines: &mut String,
    to_dep: &mut Vec<String>,
    dep: &mut FxHashMap<String,Vec<String>>,
    name: &str,
    u: &str,
) {
    if !lines.contains("extern") && !lines.contains("struct")
        || !lines.contains(";")
    {
        *lines = lines.replace(",", ";");
        if lines.contains("extern") {
            *headlines = lines.clone();
            *headlines = headlines.replace(name, "<VAR>");
        } else if !lines.contains("struct") {
            *lines = headlines.clone();
            *lines = lines.replace("<VAR>", name);
        }
    }
    for to_dep_l in to_dep.iter() {
        if let Some(get) = dep.get_mut(u) {
            get.push(to_dep_l.to_string());
        } else {
            dep.insert(u.to_string(), vec![to_dep_l.to_string()]);
        }                
    }
    to_dep.clear();
}

fn compute_dependency(pu_order: &[String], uids: FxHashMap<String, usize>, pids: FxHashMap<String, usize>,
    dep: &FxHashMap<String,Vec<String>>,
    tags: &FxHashMap<String, Vec<String>>,
    pu: &FxHashMap<String, String>,
    filename: &str,
    enumerator_to_enum: &FxHashMap<String, String>,
    system_typedefs: &[(String, String)],
    extern_functions: &FxHashMap<String, String>,
    extern_variables: &FxHashMap<String, String>,  // Bug48: extern const struct declarations
    static_funcptr_vars: &FxHashMap<String, String>) {  // Bug71: static function pointer variables

    if std::env::var("DEBUG_EXTERN").is_ok() {
        eprintln!("DEBUG compute_dependency: extern_variables.len() = {}", extern_variables.len());
    }

    // Read split configuration from environment variables
    let config = SplitConfig::from_env();
    config.log_filters();

    // Generate common header if enabled (split mode only)
    let (common_header, common_deps) = if config.is_split && config.use_common_header {
        // Identify common declarations that appear in multiple PUs
        let num_pus = uids.len();
        let threshold = if num_pus > 10 { 3 } else { 2 };

        let common_deps = CommonDeclarations::identify_common_deps(
            pu_order,
            &uids,
            &pids,
            dep,
            tags,
            pu,
            threshold,
        );

        // Generate common header if there are common dependencies
        let common_header = if !common_deps.is_empty() {
            match CommonDeclarations::generate_common_header(filename, &common_deps, pu, pu_order) {
                Ok(header_file) => {
                    eprintln!("Generated common header: {} ({} shared declarations)",
                        header_file, common_deps.len());
                    Some(header_file)
                }
                Err(e) => {
                    eprintln!("Warning: Failed to generate common header: {}", e);
                    None
                }
            }
        } else {
            None
        };

        (common_header, common_deps)
    } else {
        (None, FxHashSet::default())
    };

    // Build pre-computed structures (shared between split and non-split modes)
    let precomputed = if config.pu_filter.is_some() {
        // Use optimized version that filters transitive deps computation
        PrecomputedStructures::build_with_filter(
            pu_order, dep, tags, enumerator_to_enum, system_typedefs, pu, &config, &uids
        )
    } else {
        PrecomputedStructures::build(
            pu_order, dep, tags, enumerator_to_enum, system_typedefs, pu
        )
    };

    // PCH profitability check: PCH helps only when function bodies are the majority of the file.
    // For per-file projects (vim, kernel): headers dominate → PCH overhead exceeds savings.
    // For monolithic files (sqlite3): bodies dominate → PCH wins.
    // Threshold: src_frac >= PRECC_PCH_MIN_SRC_FRAC (default 0.5 = 50% function bodies).
    //
    // Config override: if .precc-config.toml explicitly says "pch" for this file,
    // honour that even without PRECC_PCH env var (but still check the profitability threshold).
    let cfg_entry_local: Option<PreccFileEntry> = load_config_for_file(filename)
        .and_then(|c| config_entry_for_file(&c, filename).cloned());
    let config_wants_pch = cfg_entry_local.as_ref().map(|e| e.strategy == BuildStrategy::Pch).unwrap_or(false);
    let effective_use_pch = if config.use_pch || config_wants_pch
        || std::env::var("PRECC_CLUSTER_PCH").is_ok() {
        let pch_min_src_frac: f64 = std::env::var("PRECC_PCH_MIN_SRC_FRAC")
            .ok().and_then(|s| s.parse().ok()).unwrap_or(0.5);
        // Use cached src_frac from config if available — avoids second file scan.
        let src_frac_pch = cfg_entry_local.as_ref().map(|e| e.src_frac)
            .unwrap_or_else(|| { let (_, s, _) = scan_file_properties(filename); s });
        // PRECC_CLUSTER_PCH bypasses the src_frac threshold — cluster mode works on header-heavy files
        if src_frac_pch < pch_min_src_frac && std::env::var("PRECC_CLUSTER_PCH").is_err() {
            eprintln!("PCH: skipping PCH mode for {} (src_frac={:.1}% < {:.0}% threshold) — using normal split",
                filename, src_frac_pch * 100.0, pch_min_src_frac * 100.0);
            false
        } else {
            true
        }
    } else {
        false
    };

    if config.is_split {
        // Split mode: generate separate files per PU
        if config.is_chunked {
            // Chunked mode: Group functions by UID and process each chunk together
            let mut uid_to_functions: FxHashMap<usize, Vec<String>> = FxHashMap::default();
            for (u, &uid) in uids.iter() {
                uid_to_functions.entry(uid).or_default().push(u.clone());
            }

            let unique_uids: Vec<usize> = uid_to_functions.keys().copied().collect();

            for &uid in unique_uids.iter() {
                if !config.should_process_uid(uid) {
                    continue;
                }
                let chunk_functions = uid_to_functions.get(&uid).unwrap();
                let mut necessary: FxHashSet<String> = Default::default();

                // Add all functions in this chunk as primary (need full bodies)
                let primary_functions: FxHashSet<String> = chunk_functions.iter().cloned().collect();

                // Find the maximum position index for this chunk to determine pu_order slice
                let max_j = chunk_functions.iter()
                    .filter_map(|u| pids.get(u))
                    .max()
                    .copied()
                    .unwrap_or(0);

                // Add all chunk functions to necessary set
                for u in chunk_functions {
                    necessary.insert(u.clone());
                }

                use_dependency(uid,
                    &mut necessary,
                    &pu,
                    &pu_order[0..max_j+1],
                    max_j,
                    &precomputed.position_index,
                    true,  // is_split_mode
                    common_header.as_deref(),
                    &common_deps,
                    system_typedefs,
                    Some(&primary_functions),  // chunked mode: specify primary functions
                    &extern_functions,
                    &extern_variables,
                    &static_funcptr_vars,
                    &precomputed.shared_maps,
                    &precomputed.transitive_deps,
                    &tags,
                    &precomputed.project_types,
                    &precomputed.code_identifiers,
                    &precomputed.interner,
                    &precomputed.interned_trans_deps,
                    &precomputed.interned_pos_index,
                );
            }
        } else if effective_use_pch {
            // PCH mode: generate one full common header + delta PU files (function body only)
            // This eliminates the O(N²) closure explosion: each PU is just #include + body.
            //
            // Strategy: scan the entire .i file and skip function BODIES.
            // Include everything else: types, struct defs, variable declarations, inline fns.
            // A "function body" starts with "{" at column 0 preceded by a non-indented ")"
            // and ends with "}" at column 0 (the matching close brace).
            // Variable initializers (= {...};) are NOT function bodies.
            //
            // This generates a PCH containing all types throughout the file (not just the
            // preamble), avoiding the "unknown type name" errors for types defined after
            // the first function. The static variable initializers ARE included (they're
            // valid in a header since they're defined once in the PCH). Delta PU files
            // just do `#include "pch.h"` + function body, with no redefinition conflicts
            // because the PCH contains declarations only (for non-inline fns).
            let pch_header_path = format!("{}.pch.h", filename);

            // Collect all function names for forward declarations
            let _fn_forward_decls: Vec<(usize, String)> = Vec::new(); // unused placeholder
            // Function names defined in the PCH (inline fns + static vars in initializers)
            let mut preamble_defined_fns_outer: FxHashSet<String> = FxHashSet::default();
            // Track which functions have full bodies in the PCH (inline functions)
            // These should NOT get delta PU files
            let mut preamble_end_line: usize = 0; // unused but kept for compat

            // Scan the entire .i file, skip non-inline function bodies.
            // Output: all typedefs + struct/enum defs + inline fn bodies + var declarations.
            // This produces a valid PCH header with ALL types, no forward-reference issues.
            if let Ok(content) = std::fs::read_to_string(filename) {
                let lines: Vec<&str> = content.lines().collect();
                let n = lines.len();

                // Count net brace depth in a line, skipping '{'/'}' inside string/char literals and comments.
                let count_braces = |s: &str| -> i32 {
                    let mut depth = 0i32;
                    let bytes = s.as_bytes();
                    let len = bytes.len();
                    let mut j = 0usize;
                    while j < len {
                        match bytes[j] {
                            b'"' => {
                                j += 1;
                                while j < len {
                                    if bytes[j] == b'\\' { j += 2; continue; }
                                    if bytes[j] == b'"' { break; }
                                    j += 1;
                                }
                            }
                            b'\'' => {
                                j += 1;
                                while j < len {
                                    if bytes[j] == b'\\' { j += 2; continue; }
                                    if bytes[j] == b'\'' { break; }
                                    j += 1;
                                }
                            }
                            b'/' if j + 1 < len && bytes[j+1] == b'*' => {
                                j += 2;
                                while j + 1 < len {
                                    if bytes[j] == b'*' && bytes[j+1] == b'/' { j += 2; break; }
                                    j += 1;
                                }
                                continue;
                            }
                            b'/' if j + 1 < len && bytes[j+1] == b'/' => {
                                break; // line comment: rest of line ignored
                            }
                            b'{' => { depth += 1; }
                            b'}' => { depth -= 1; }
                            _ => {}
                        }
                        j += 1;
                    }
                    depth
                };

                let linemarker_re = regex::Regex::new(r#"^# \d+ ""#).unwrap();
                let control_flow_re = regex::Regex::new(
                    r"\b(if|while|for|switch|do|return|else)\s*\("
                ).unwrap();
                let fn_name_re = regex::Regex::new(r"^([a-zA-Z_][a-zA-Z0-9_]*)\s*\(").unwrap();
                let style_d_start_re = regex::Regex::new(
                    r"^\s*(if|while|for|switch|do|else)\s*\("
                ).unwrap();
                let id_end_re = regex::Regex::new(r"([a-zA-Z_][a-zA-Z0-9_]*)$").unwrap();
                let mut preamble_defined_fns: FxHashSet<String> = FxHashSet::default();

                // Collect output as Vec<String> so we can backtrack for Style C sigs
                let mut out_lines: Vec<String> = Vec::with_capacity(n / 4);
                // Track where the current function signature started in out_lines
                // (for Style C backtracking). None means we're not in a potential sig.
                let mut sig_start_out_idx: Option<usize> = None;

                let mut i = 0usize;
                let mut in_fn_body = false;    // skipping a non-inline function body
                let mut fn_brace_depth = 0i32; // brace depth inside skipped fn body
                let mut in_static_init = false; // skipping a static variable initializer { ... }
                let mut static_init_depth = 0i32; // brace depth for static init
                let mut inline_body_depth = 0i32; // brace depth inside an included inline fn body
                let mut static_init_elem_count = 0i32; // top-level element count for [] sizing
                let mut static_init_out_idx: Option<usize> = None; // position in out_lines for fixup
                // Track where sig started in the raw `lines[]` array (for control-flow context check)
                let mut sig_start_line_idx: Option<usize> = None;
                // Track multi-line function signatures: when a col-0 line has `(` but doesn't close
                // with `)` on the same line, subsequent indented lines continue the parameter list.
                // `pending_fn_sig_start` = the line index where the signature began.
                let mut pending_fn_sig_start: Option<usize> = None;

                while i < n {
                    let line = lines[i];

                    if in_fn_body {
                        // Count braces (skipping string/char literals and comments)
                        fn_brace_depth += count_braces(line);
                        if fn_brace_depth <= 0 {
                            in_fn_body = false;
                            fn_brace_depth = 0;
                            sig_start_out_idx = None;
                            sig_start_line_idx = None;
                        }
                        i += 1;
                        continue;
                    }

                    // Track brace depth inside included inline function bodies.
                    // When inline_body_depth > 0 we're inside an inline fn body being kept in the PCH.
                    // In that context, local variable initializers like `swp_entry_t swap = {` must NOT
                    // trigger in_static_init (they're local vars, not static globals).
                    if inline_body_depth > 0 {
                        inline_body_depth += count_braces(line);
                        if inline_body_depth <= 0 {
                            inline_body_depth = 0;
                        }
                    }

                    // Skip static variable initializers (multi-line = { ... })
                    if in_static_init {
                        // Count top-level commas (depth==1 means directly inside the outer braces)
                        {
                            let bytes = line.as_bytes();
                            let len = bytes.len();
                            let mut j = 0usize;
                            let mut d = static_init_depth;
                            while j < len {
                                match bytes[j] {
                                    b'"' => { j += 1; while j < len { if bytes[j] == b'\\' { j += 2; continue; } if bytes[j] == b'"' { break; } j += 1; } }
                                    b'\'' => { j += 1; while j < len { if bytes[j] == b'\\' { j += 2; continue; } if bytes[j] == b'\'' { break; } j += 1; } }
                                    b'/' if j + 1 < len && bytes[j+1] == b'*' => { j += 2; while j + 1 < len { if bytes[j] == b'*' && bytes[j+1] == b'/' { j += 2; break; } j += 1; } continue; }
                                    b'/' if j + 1 < len && bytes[j+1] == b'/' => { break; }
                                    b'{' => { d += 1; }
                                    b'}' => { d -= 1; }
                                    b',' if d == 1 => { static_init_elem_count += 1; }
                                    _ => {}
                                }
                                j += 1;
                            }
                        }
                        static_init_depth += count_braces(line);
                        if static_init_depth <= 0 {
                            // Fix up the declaration in out_lines: replace [] with [N]
                            if let Some(idx) = static_init_out_idx {
                                if idx < out_lines.len() {
                                    let entry = &out_lines[idx].clone();
                                    if let Some(bracket_pos) = entry.find("[]") {
                                        let new_entry = format!("{}[{}]{}", &entry[..bracket_pos], static_init_elem_count, &entry[bracket_pos+2..]);
                                        out_lines[idx] = new_entry;
                                    }
                                }
                            }
                            in_static_init = false;
                            static_init_depth = 0;
                            static_init_elem_count = 0;
                            static_init_out_idx = None;
                        }
                        i += 1;
                        continue;
                    }

                    let lt = line.trim();

                    // Pattern: "} name[] = {" — closes an anonymous struct/union and starts
                    // a static initializer. We output "} name[];" to keep the type closing,
                    // then skip the initializer body.
                    if (lt.starts_with("} ") || lt.starts_with("}")) && lt.ends_with("= {") {
                        // Find the '=' position
                        if let Some(eq) = lt.rfind("= {") {
                            let decl_part = lt[..eq].trim_end();
                            if !decl_part.is_empty() {
                                // Output the declaration as "} name[];" (closing the struct)
                                if !linemarker_re.is_match(line) {
                                    out_lines.push(format!("{};", decl_part));
                                }
                            }
                        }
                        in_static_init = true;
                        static_init_depth = 1;
                        static_init_elem_count = 0;
                        static_init_out_idx = if out_lines.is_empty() { None } else { Some(out_lines.len()-1) };
                        sig_start_out_idx = None;
                        i += 1;
                        continue;
                    }

                    // Skip single-line static initializers that reference forward-declared symbols:
                    // e.g. "static const X *foo = &symbol[...];" or "... = (cast)expr;"
                    // These cause "undeclared" errors in the PCH since the referenced symbol
                    // may not have been declared yet.
                    //
                    // NOTE: Skip this check for single-line brace-balanced inline function definitions
                    // (Style D), which also start with "static" but contain function bodies, not
                    // variable initializers. E.g. "static inline void foo() { bar = 1; }" — the `=`
                    // inside the function body must not be treated as a variable initializer.
                    let is_col0_early = !line.starts_with('\t') && !line.starts_with("   ");
                    let is_fn_body_d_early = is_col0_early
                        && lt.ends_with('}')
                        && lt.contains('(')
                        && lt.contains('{')
                        && count_braces(lt) == 0
                        && !lt.starts_with("typedef") && !lt.starts_with("struct")
                        && !lt.starts_with("union") && !lt.starts_with("enum");
                    {
                        let is_static_decl = !is_fn_body_d_early
                            && inline_body_depth == 0
                            && (lt.starts_with("static ") || lt.starts_with("const "));
                        let has_init = {
                            // find '=' not inside '<' or '>' angle brackets, not part of '=='
                            let mut eq_pos = None;
                            let bytes = lt.as_bytes();
                            let mut j = 0usize;
                            while j < bytes.len().saturating_sub(1) {
                                if bytes[j] == b'=' && bytes[j+1] != b'=' && (j == 0 || bytes[j-1] != b'!') {
                                    eq_pos = Some(j);
                                    break;
                                }
                                j += 1;
                            }
                            eq_pos
                        };
                        if is_static_decl {
                            if let Some(eq) = has_init {
                                // If there are unmatched '{' before the '=', we're inside a fn body.
                                // Don't treat this as a variable initializer.
                                let brace_depth_at_eq = count_braces(&lt[..eq]);
                                if brace_depth_at_eq > 0 && count_braces(lt) == 0 {
                                    // '=' is inside a function body and braces are balanced (single-line
                                    // complete fn body, possibly followed by more static declarations).
                                    // Extract forward declarations for any functions defined on this line.
                                    // Pattern: `} static [attr...] type funcname(params) {`
                                    // We find all `} static` breakpoints and extract decls.
                                    if !linemarker_re.is_match(line) {
                                        let fn_decl_re_local = regex::Regex::new(
                                            r"(?:^|[}]\s*)static\s+(?:__attribute__\(\([^)]*\)\)\s*)*(?:[a-zA-Z_][a-zA-Z0-9_\s\*]*)\b([a-zA-Z_][a-zA-Z0-9_]*)\s*\("
                                        ).unwrap();
                                        let mut decls_out: Vec<String> = Vec::new();
                                        let mut search_start = 0usize;
                                        let lt_bytes = lt.as_bytes();
                                        let lt_len = lt_bytes.len();
                                        // Split on "} static" boundaries
                                        let mut segments: Vec<&str> = Vec::new();
                                        let mut seg_start = 0usize;
                                        let mut i2 = 0usize;
                                        while i2 < lt_len {
                                            if lt_bytes[i2] == b'}' {
                                                let rest = lt[i2+1..].trim_start();
                                                if rest.starts_with("static ") || rest.starts_with("static\t") {
                                                    segments.push(&lt[seg_start..i2]);
                                                    seg_start = i2 + 1 + (lt[i2+1..].len() - rest.len());
                                                }
                                            }
                                            i2 += 1;
                                        }
                                        segments.push(&lt[seg_start..]);
                                        let n_segs = segments.len();
                                        for (si, seg) in segments.iter().enumerate() {
                                            let seg = seg.trim();
                                            if !seg.starts_with("static ") { continue; }
                                            // Find the boundary: '(' for function, '=' for variable
                                            // Case 1: function definition — has '(' before '{'
                                            // Case 2: variable initializer — has '=' before '{'
                                            let fn_paren = seg.find('(');
                                            let init_eq = seg.bytes().enumerate()
                                                .find(|&(j, b)| b == b'=' &&
                                                    seg.as_bytes().get(j+1).copied().unwrap_or(0) != b'=' &&
                                                    (j == 0 || seg.as_bytes()[j-1] != b'!'))
                                                .map(|(j, _)| j);
                                            let brace_pos = seg.find('{');
                                            if let Some(bp) = brace_pos {
                                                let sig_part = seg[..bp].trim_end();
                                                let is_fn = fn_paren.map_or(false, |fp| fp < bp);
                                                let is_var = init_eq.map_or(false, |ep| ep < bp && fn_paren.map_or(true, |fp| ep < fp));
                                                if is_fn && !sig_part.ends_with('=')
                                                    && sig_part.trim_end().ends_with(')')
                                                {
                                                    // Function: emit declaration
                                                    decls_out.push(format!("{};", sig_part));
                                                } else if is_var {
                                                    // Variable with initializer: emit declaration without initializer
                                                    let decl_part = sig_part[..init_eq.unwrap()].trim_end();
                                                    if !decl_part.is_empty() {
                                                        decls_out.push(format!("{};", decl_part));
                                                    }
                                                }
                                            }
                                            // Note: segments ending with ';' or without '{'/';' are NOT
                                            // emitted here. Forward declarations will appear when the body
                                            // is processed normally. The last open-signature segment (no '{')
                                            // is handled by Style A detection on the next line.
                                        }
                                        for decl in decls_out {
                                            out_lines.push(decl);
                                        }
                                    }
                                    i += 1;
                                    continue;
                                }
                                let after_eq = lt[eq+1..].trim_start();
                                // Multi-line: ends with "= {" → skip body, output declaration only
                                if after_eq == "{" || lt.ends_with("= {") || lt.ends_with("={") {
                                    if !lt.ends_with("){") && !lt.ends_with(") {") {
                                        // Output the declaration without the initializer
                                        let decl = lt[..eq].trim_end();
                                        if !decl.is_empty() && !linemarker_re.is_match(line) {
                                            out_lines.push(format!("{};", decl));
                                        }
                                        in_static_init = true;
                                        static_init_depth = 1;
                                        static_init_elem_count = 0;
                                        static_init_out_idx = if out_lines.is_empty() { None } else { Some(out_lines.len()-1) };
                                        sig_start_out_idx = None;
                                        i += 1;
                                        continue;
                                    }
                                }
                                // Single-line: starts with & or ( → output declaration without initializer
                                if after_eq.starts_with('&') || after_eq.starts_with('(') {
                                    let decl = lt[..eq].trim_end();
                                    if !decl.is_empty() && !linemarker_re.is_match(line) {
                                        out_lines.push(format!("{};", decl));
                                    }
                                    i += 1;
                                    continue;
                                }
                                // Starts with { — could be single-line or multi-line initializer.
                                // Check brace balance (respecting string/char literals).
                                if after_eq.starts_with('{') {
                                    let net_braces = count_braces(after_eq);
                                    if net_braces <= 0 {
                                        // Balanced: single-line initializer; output declaration only.
                                        // If it's an array (decl ends with []), count elements to fill in size.
                                        let raw_decl = lt[..eq].trim_end();
                                        let decl = if raw_decl.ends_with("[]") {
                                            // Count top-level comma-separated elements
                                            let inner = after_eq.trim_start_matches('{');
                                            let inner = &inner[..inner.rfind('}').unwrap_or(inner.len())];
                                            let mut depth = 0i32;
                                            let mut count = if inner.trim().is_empty() { 0 } else { 1 };
                                            for ch in inner.chars() {
                                                match ch {
                                                    '{' => depth += 1,
                                                    '}' => depth -= 1,
                                                    ',' if depth == 0 => count += 1,
                                                    _ => {}
                                                }
                                            }
                                            raw_decl[..raw_decl.len()-2].to_string() + &format!("[{}]", count)
                                        } else {
                                            raw_decl.to_string()
                                        };
                                        if !decl.is_empty() && !linemarker_re.is_match(line) {
                                            out_lines.push(format!("{};", decl));
                                        }
                                        // Check if there are additional statements after the closing }
                                        // on the same line (e.g., multi-statement macro-expanded lines)
                                        if let Some(close_pos) = after_eq.rfind('}') {
                                            let rest = after_eq[close_pos+1..].trim_start_matches(';').trim();
                                            if !rest.is_empty() && rest.starts_with("static ") {
                                                // There are more static declarations on this line.
                                                // Find '=' in the rest for another initializer
                                                let rest_eq = rest.as_bytes().windows(2)
                                                    .position(|w| w[0] == b'=' && w[1] != b'=')
                                                    .and_then(|p| if p == 0 || rest.as_bytes()[p-1] != b'!' { Some(p) } else { None });
                                                if let Some(req) = rest_eq {
                                                    let rest_decl = rest[..req].trim_end();
                                                    if !rest_decl.is_empty() && !linemarker_re.is_match(line) {
                                                        out_lines.push(format!("{};", rest_decl));
                                                    }
                                                } else {
                                                    // No '=' — it's a plain declaration or function def
                                                    // Try to output as-is if it ends with ';'
                                                    if rest.ends_with(';') && !linemarker_re.is_match(line) {
                                                        out_lines.push(rest.to_string());
                                                    }
                                                }
                                            }
                                        }
                                        i += 1;
                                        continue;
                                    } else {
                                        // Unbalanced (starts with { but not closed): multi-line
                                        let decl = lt[..eq].trim_end();
                                        if !decl.is_empty() && !linemarker_re.is_match(line) {
                                            out_lines.push(format!("{};", decl));
                                        }
                                        in_static_init = true;
                                        // Depth = net_braces already opened on this line
                                        static_init_depth = net_braces;
                                        static_init_elem_count = 0;
                                        static_init_out_idx = if out_lines.is_empty() { None } else { Some(out_lines.len()-1) };
                                        sig_start_out_idx = None;
                                        sig_start_line_idx = None;
                                        i += 1;
                                        continue;
                                    }
                                }
                            }
                        } else if !lt.starts_with("typedef") && !lt.starts_with("struct")
                               && !lt.starts_with("union") && !lt.starts_with("enum")
                               && !lt.starts_with("//") && !lt.starts_with("/*") {
                            // Non-typedef non-struct: check for multi-line = {
                            // Skip inside inline function bodies (local var decls look like static inits)
                            if inline_body_depth == 0
                                && (lt.ends_with("= {") || lt.ends_with("={"))
                                && !lt.ends_with("){") && !lt.ends_with(") {") {
                                in_static_init = true;
                                static_init_depth = 1;
                                static_init_elem_count = 0;
                                static_init_out_idx = None;
                                sig_start_out_idx = None;
                                i += 1;
                                continue;
                            }
                        }
                    }

                    // Style F: packed SYSCALL_DEFINE line with balanced braces ending with ')'
                    // e.g. " static long __se_compat_sys_mq_open(...); ... { return ... ; }
                    //        static inline ... long __do_compat_sys_mq_open(...)"
                    // The line contains complete function bodies (brace-balanced) but ends with an
                    // open trailing function signature. Extract declarations; skip the line.
                    // The body of the trailing fn comes on the next line (handled by Style A).
                    if !in_fn_body && !in_static_init
                        && lt.starts_with("static ")
                        && lt.ends_with(')')
                        && lt.contains('{')
                        && lt.contains('}')
                        && count_braces(lt) == 0
                        && !linemarker_re.is_match(line)
                    {
                        let lt_bytes_f = lt.as_bytes();
                        let lt_len_f = lt_bytes_f.len();
                        let mut seg_start_f = 0usize;
                        let mut if_f = 0usize;
                        let mut segs_f: Vec<&str> = Vec::new();
                        while if_f < lt_len_f {
                            if lt_bytes_f[if_f] == b'}' {
                                let rest_f = lt[if_f+1..].trim_start();
                                if rest_f.starts_with("static ") || rest_f.starts_with("static\t") {
                                    segs_f.push(&lt[seg_start_f..if_f]);
                                    seg_start_f = if_f + 1 + (lt[if_f+1..].len() - rest_f.len());
                                }
                            }
                            if_f += 1;
                        }
                        segs_f.push(&lt[seg_start_f..]);
                        // Last segment has no '{' (it's the trailing open sig) — skip it.
                        // For other segments, emit function declarations (sig before '{', ends with ')').
                        let has_any_complete_seg = segs_f.iter().any(|s| s.contains('{'));
                        if has_any_complete_seg {
                            for seg_f in &segs_f {
                                let seg_f = seg_f.trim();
                                if let Some(bp_f) = seg_f.find('{') {
                                    let sig_f = seg_f[..bp_f].trim_end();
                                    if !sig_f.is_empty() && sig_f.contains('(')
                                        && sig_f.trim_end().ends_with(')')
                                    {
                                        out_lines.push(format!("{};", sig_f));
                                    }
                                }
                            }
                            sig_start_out_idx = None;
                            sig_start_line_idx = None;
                            pending_fn_sig_start = None;
                            i += 1;
                            continue;
                        }
                    }

                    // Check if this line starts a function body.
                    // Style A: '{' alone at column 0, preceded by non-indented line ending ')'
                    // Style B: line ends with ){ and starts at col 0, contains (
                    // Style C: line is just "){ " — closes a multi-line parameter list
                    // col0 = not tab-indented and not indented more than 2 spaces
                    // (2-space indent can appear for macro-generated functions in .i files)
                    let is_col0 = !line.starts_with('\t') && !line.starts_with("   ");
                    // For Style A, find the preceding non-empty line (may be separated by blank lines)
                    let style_a_prev_idx = if line == "{" && i > 0 {
                        let mut pi = i - 1;
                        while pi > 0 && lines[pi].trim().is_empty() { pi -= 1; }
                        pi
                    } else { i.saturating_sub(1) };
                    let is_fn_body_start_a = line == "{" && i > 0 && {
                        let prev = lines[style_a_prev_idx];
                        let prev_trimmed = prev.trim();
                        let cond1 = prev_trimmed.ends_with(')') || prev_trimmed.ends_with(") ");
                        let cond2 = !prev.starts_with('\t') && !prev.starts_with("  ")
                                || sig_start_line_idx.is_some() || pending_fn_sig_start.is_some();
                        cond1 && cond2
                    };
                    let is_fn_body_start_b = is_col0
                        && (lt.ends_with("){") || lt.ends_with(") {"))
                        && lt.contains('(')
                        && !control_flow_re.is_match(lt);
                    // Style C: ){ alone at col 0 — multi-line param list ending
                    let is_fn_body_start_c = is_col0
                        && (lt == "){" || lt == ") {");
                    // Style D: complete single-line function body, e.g. "static int foo(){ return 0; }"
                    // Col-0, contains '(' and '{', ends with '}', brace-balanced (net 0), not control flow at start
                    let is_fn_body_start_d = is_col0
                        && lt.ends_with('}')
                        && lt.contains('(')
                        && lt.contains('{')
                        && count_braces(lt) == 0
                        && !style_d_start_re.is_match(lt)
                        && !lt.starts_with("typedef") && !lt.starts_with("struct")
                        && !lt.starts_with("union") && !lt.starts_with("enum");
                    // Style E: multi-line param list ending with body inline, e.g.:
                    //   (line N, col 0)  "static inline void foo(int a,"
                    //   (line N+1, indented) "     int b) { }"
                    // Detected when: pending_fn_sig_start is set, current line contains "){" or ") {"
                    // and the line ends with '}' and brace-balanced (or ends with '{' for multi-line body).
                    let is_fn_body_start_e = pending_fn_sig_start.is_some()
                        && !is_col0
                        && (lt.contains(") {") || lt.contains("){"))
                        && !control_flow_re.is_match(lt);

                    if is_fn_body_start_a || is_fn_body_start_b || is_fn_body_start_c || is_fn_body_start_d || is_fn_body_start_e {
                        // Use sig_start_line_idx to bound the context — avoids false matches
                        // from control-flow keywords in previous (already-skipped) function bodies.
                        let ctrl_ctx_start = if is_fn_body_start_a && i > 0 {
                            // For Style A, the sig is the single line before '{';
                            // use only that line for control-flow check to avoid cross-function contamination.
                            i - 1
                        } else {
                            sig_start_line_idx.unwrap_or_else(|| i.saturating_sub(8))
                        };
                        // For inline detection, use a slightly broader context (up to 4 lines back)
                        // to catch "static __inline rettype\nfunc_name(...)\n{" patterns.
                        let inline_ctx_start = sig_start_line_idx.unwrap_or_else(|| i.saturating_sub(4));
                        let inline_ctx_raw: String = lines[inline_ctx_start..=i].join(" ");
                        // For Style A ('{' alone), the preceding line may be a SYSCALL_DEFINE macro
                        // expansion that packs multiple declarations (including unrelated inline ones)
                        // onto one line. In that case, take only the LAST semicolon-delimited segment
                        // so we don't pick up `inline` from a different declaration.
                        let inline_ctx: &str = if is_fn_body_start_a && i > 0 {
                            let prev_line = lines[style_a_prev_idx].trim();
                            // For packed SYSCALL_DEFINE/TRACE_EVENT lines (containing ';'), take only
                            // the LAST semicolon-delimited segment so we don't pick up `inline` from
                            // a different declaration on the same line.
                            // For regular multi-line function signatures (no ';' in preceding line),
                            // use the broader inline_ctx_raw (which includes the return type line).
                            if prev_line.contains(';') {
                                let after = prev_line[..prev_line.rfind(';').unwrap()].trim();
                                if let Some(prev_semi) = after.rfind(';') {
                                    &prev_line[prev_semi+1..]
                                } else {
                                    prev_line
                                }
                            } else {
                                // No ';' in preceding line: multi-line sig. Use broad context.
                                &inline_ctx_raw
                            }
                        } else {
                            &inline_ctx_raw
                        };
                        let is_inline = inline_ctx.contains("__inline") || inline_ctx.contains(" inline ")
                            || inline_ctx.contains("__forceinline");
                        let is_control_flow = if is_fn_body_start_b || is_fn_body_start_d {
                            // B and D: already checked control_flow_re on lt in the detection above
                            false
                        } else if i > 0 {
                            let prev = lines[style_a_prev_idx];
                            let prev_trimmed_cf = prev.trim();
                            if is_fn_body_start_a {
                                // For Style A with a packed preceding line (SYSCALL_DEFINE / TRACE_EVENT),
                                // only check control flow in the LAST segment (after the last "} static").
                                // Earlier segments may contain if/while/for inside inline function bodies.
                                let bytes = prev_trimmed_cf.as_bytes();
                                let mut last_boundary = 0usize;
                                let mut bi = 0usize;
                                while bi < bytes.len() {
                                    if bytes[bi] == b'}' {
                                        let rest = prev_trimmed_cf[bi+1..].trim_start();
                                        if rest.starts_with("static ") || rest.starts_with("static\t") {
                                            last_boundary = bi + 1;
                                        }
                                    }
                                    bi += 1;
                                }
                                let last_seg = &prev_trimmed_cf[last_boundary..];
                                control_flow_re.is_match(last_seg)
                            } else {
                                let prev_ctx: String = lines[ctrl_ctx_start..i].join(" ");
                                control_flow_re.is_match(&prev_ctx) || control_flow_re.is_match(prev_trimmed_cf)
                            }
                        } else {
                            false
                        };
                        if !is_control_flow {
                            if !is_inline {
                                // Non-inline: skip body.
                                // Style A: signature is already in out_lines without ';'
                                //   → find last non-empty line and append ';' to make it a declaration
                                // Style C: remove multi-line signature from output entirely
                                if is_fn_body_start_a {
                                    // Convert the signature line (already output) to a declaration
                                    if let Some(last) = out_lines.iter_mut().rev().find(|l| !l.trim().is_empty()) {
                                        if !last.trim_end().ends_with(';') && !last.trim_end().ends_with('{') {
                                            last.push(';');
                                        }
                                    }
                                } else if is_fn_body_start_c {
                                    // Backtrack: remove the multi-line signature lines already output,
                                    // then emit them joined as a single declaration.
                                    // The current line is '){' or ') {' — the ')' closes the function param list.
                                    if let Some(sig_start) = sig_start_out_idx {
                                        let sig_lines: Vec<String> = out_lines.drain(sig_start..).collect();
                                        let joined = sig_lines.iter()
                                            .map(|s| s.trim())
                                            .filter(|s| !s.is_empty())
                                            .collect::<Vec<_>>()
                                            .join(" ");
                                        // The ')' that closes the param list is on the current '){ ' line.
                                        // Always append it since sig_lines contain only the open paren + params.
                                        let decl = format!("{});", joined.trim_end());
                                        if !linemarker_re.is_match(line) {
                                            out_lines.push(decl);
                                        }
                                    }
                                }
                                if is_fn_body_start_d {
                                    // Single-line complete body (or multiple bodies on one line).
                                    // Split on "} static" boundaries to handle TRACE_EVENT-style
                                    // macro expansions that emit multiple function bodies per line.
                                    if !linemarker_re.is_match(line) {
                                        let lt_bytes2 = lt.as_bytes();
                                        let lt_len2 = lt_bytes2.len();
                                        let mut seg_start2 = 0usize;
                                        let mut i3 = 0usize;
                                        let mut segs2: Vec<&str> = Vec::new();
                                        while i3 < lt_len2 {
                                            if lt_bytes2[i3] == b'}' {
                                                let rest2 = lt[i3+1..].trim_start();
                                                if rest2.starts_with("static ") || rest2.starts_with("static\t") {
                                                    segs2.push(&lt[seg_start2..i3]);
                                                    seg_start2 = i3 + 1 + (lt[i3+1..].len() - rest2.len());
                                                }
                                            }
                                            i3 += 1;
                                        }
                                        segs2.push(&lt[seg_start2..]);
                                        for seg2 in &segs2 {
                                            let seg2 = seg2.trim();
                                            if let Some(bp2) = seg2.find('{') {
                                                let sig2 = seg2[..bp2].trim_end();
                                                // Only emit if it looks like a function signature:
                                                // ends with ')' and contains '(' (not a struct/variable init)
                                                if !sig2.is_empty() && sig2.contains('(')
                                                    && sig2.trim_end().ends_with(')')
                                                {
                                                    out_lines.push(format!("{};", sig2));
                                                }
                                            }
                                        }
                                    }
                                    sig_start_out_idx = None;
                                    sig_start_line_idx = None;
                                    pending_fn_sig_start = None;
                                    i += 1;
                                    continue;
                                }
                                if is_fn_body_start_e && count_braces(lt) == 0 {
                                    // Style E, complete body on this line: backtrack sig from out_lines
                                    // and emit a declaration. E.g. "     int b) { }" → reconstruct decl.
                                    if let Some(sig_start) = sig_start_out_idx {
                                        let sig_lines: Vec<String> = out_lines.drain(sig_start..).collect();
                                        let joined = sig_lines.iter()
                                            .map(|s| s.trim())
                                            .filter(|s| !s.is_empty())
                                            .collect::<Vec<_>>()
                                            .join(" ");
                                        // Current line has "...) { }" — extract up to first '{'
                                        let before_brace = lt.find('{')
                                            .map(|p| lt[..p].trim_end())
                                            .unwrap_or(lt);
                                        let decl = format!("{} {});", joined.trim_end(), before_brace);
                                        if !linemarker_re.is_match(line) {
                                            out_lines.push(decl);
                                        }
                                    }
                                    sig_start_out_idx = None;
                                    sig_start_line_idx = None;
                                    pending_fn_sig_start = None;
                                    i += 1;
                                    continue;
                                }
                                if is_fn_body_start_b {
                                    // Style B: line is "rettype func(params){" — output as declaration
                                    // Strip the trailing "{" (and possibly " {")
                                    let sig = lt.trim_end_matches('{').trim_end().to_string();
                                    if !sig.is_empty() && !linemarker_re.is_match(line) {
                                        out_lines.push(format!("{};", sig));
                                    }
                                    // Body is handled below (in_fn_body = true)
                                }
                                in_fn_body = true;
                                if is_fn_body_start_b {
                                    fn_brace_depth = count_braces(line);
                                    if fn_brace_depth <= 0 {
                                        in_fn_body = false;
                                        fn_brace_depth = 0;
                                    }
                                } else {
                                    fn_brace_depth = 1;
                                }
                                sig_start_out_idx = None;
                                sig_start_line_idx = None;
                                pending_fn_sig_start = None;
                                i += 1;
                                continue;
                            } else {
                                // Inline: include in PCH, record its name.
                                // Use a smarter extraction: find the identifier just before '('
                                // (skipping over __attribute__((...))).
                                let sig_line = if is_fn_body_start_b || is_fn_body_start_d {
                                    lt
                                } else if is_fn_body_start_e {
                                    // Style E: sig starts at pending_fn_sig_start line
                                    pending_fn_sig_start.map(|si| lines[si].trim()).unwrap_or(lt)
                                } else {
                                    // Style A: use the actual preceding non-empty line
                                    lines[style_a_prev_idx].trim()
                                };
                                // Extract function name: find last identifier before the param-opening '('
                                // (the outermost '(' after skipping __attribute__((...))-style parens)
                                let fn_name_opt = {
                                    let bytes = sig_line.as_bytes();
                                    let len = bytes.len();
                                    // Find the first '(' that directly precedes the parameter list
                                    // (skip nested __attribute__((...))-style blocks)
                                    let mut param_open: Option<usize> = None;
                                    let mut depth = 0i32;
                                    let mut j = 0usize;
                                    while j < len {
                                        if bytes[j] == b'(' {
                                            depth += 1;
                                            if depth == 1 {
                                                // Is this followed immediately (after spaces) by a non-'('
                                                // or does it look like __attribute__(()?
                                                // Heuristic: if depth goes back to 0 and then we see another '(',
                                                // that's the param-opening one.
                                                // For now, record all depth-1 openings; the LAST one is likely params.
                                                param_open = Some(j);
                                            }
                                        } else if bytes[j] == b')' {
                                            depth -= 1;
                                        }
                                        j += 1;
                                    }
                                    // param_open is the last depth-1 '(' = the function param list '('
                                    param_open.and_then(|open_pos| {
                                        // Backtrack from open_pos to find the identifier
                                        let before = sig_line[..open_pos].trim_end();
                                        // Find last identifier
                                        id_end_re.captures(before).map(|c| c[1].to_string())
                                    })
                                };
                                if let Some(fname) = fn_name_opt {
                                    preamble_defined_fns.insert(fname.clone());
                                    // For Style A: if the preceding line was a packed multi-declaration
                                    // line (contains multiple ';' separated declarations), GCC may
                                    // attribute the following '{' to the wrong function. Inject the
                                    // inline function's own signature to clarify.
                                    let prev_is_packed = is_fn_body_start_a && i > 0 && {
                                        let p = lines[style_a_prev_idx].trim();
                                        // Packed if has ';' AND the part after the last ';' looks like
                                        // a function signature (contains '(' but no ';')
                                        p.contains(';') && {
                                            let after_last = p.rsplit(';').next().unwrap_or("").trim();
                                            after_last.contains('(') && !after_last.contains(';')
                                        }
                                    };
                                    if prev_is_packed && !linemarker_re.is_match(line) {
                                        // The packed SYSCALL_DEFINE line has the trailing inline fn
                                        // embedded in the same PU as __se_sys_* — so the body will
                                        // appear in the bundle. Emit ONLY a declaration in the PCH
                                        // (with ';') to avoid redefinition when bundle is compiled.
                                        let sig_raw = sig_line;
                                        let sr_bytes = sig_raw.as_bytes();
                                        let sr_len = sr_bytes.len();
                                        let mut last_seg_start = 0usize;
                                        let mut si2 = 0usize;
                                        while si2 < sr_len {
                                            if sr_bytes[si2] == b'}' {
                                                let r2 = sig_raw[si2+1..].trim_start();
                                                if r2.starts_with("static ") || r2.starts_with("static\t") {
                                                    last_seg_start = si2 + 1 + (sig_raw[si2+1..].len() - r2.len());
                                                }
                                            }
                                            si2 += 1;
                                        }
                                        let last_sig = sig_raw[last_seg_start..].trim();
                                        if last_sig.ends_with(')') && last_sig.contains('(') {
                                            // Emit as declaration (with ';'), skip the body.
                                            out_lines.push(format!("{};", last_sig));
                                        }
                                        // Skip the body (it's in the bundle via __se_sys_* PU).
                                        sig_start_out_idx = None;
                                        sig_start_line_idx = None;
                                        pending_fn_sig_start = None;
                                        in_fn_body = true;
                                        fn_brace_depth = 1; // '{' is the current line
                                        i += 1;
                                        continue;
                                    }
                                }
                                sig_start_out_idx = None;
                                sig_start_line_idx = None;
                                pending_fn_sig_start = None;
                                // Track depth inside this inline body so we don't misparse
                                // local variable initializers (e.g. `swp_entry_t swap = {`) inside it.
                                if is_fn_body_start_b || is_fn_body_start_d {
                                    inline_body_depth = count_braces(lt);
                                } else {
                                    // Style A/C/E: body opens on this line or next '{'
                                    inline_body_depth = count_braces(lt);
                                    if inline_body_depth <= 0 { inline_body_depth = 1; }
                                }
                                // For Style D (packed inline line ending with ')' or '}'), the function
                                // body(ies) are fully contained on this line (count_braces == 0).
                                // Emit declarations for each segment and skip the line.
                                // For packed lines ending with an open function signature (no body on
                                // this line, body follows on next line), extract declarations from
                                // complete segments and skip the packed line. The trailing signature
                                // will be re-emitted by the Style A injection on the next '{' line.
                                let is_packed_open_sig = count_braces(lt) == 0
                                    && lt.ends_with(')')
                                    && lt.contains('{')
                                    && lt.starts_with("static ");
                                if is_packed_open_sig {
                                    if !linemarker_re.is_match(line) {
                                        let lt_bytes2 = lt.as_bytes();
                                        let lt_len2 = lt_bytes2.len();
                                        let mut seg_start2 = 0usize;
                                        let mut i3 = 0usize;
                                        let mut segs2: Vec<&str> = Vec::new();
                                        while i3 < lt_len2 {
                                            if lt_bytes2[i3] == b'}' {
                                                let rest2 = lt[i3+1..].trim_start();
                                                if rest2.starts_with("static ") || rest2.starts_with("static\t") {
                                                    segs2.push(&lt[seg_start2..i3]);
                                                    seg_start2 = i3 + 1 + (lt[i3+1..].len() - rest2.len());
                                                }
                                            }
                                            i3 += 1;
                                        }
                                        segs2.push(&lt[seg_start2..]);
                                        for seg2 in &segs2 {
                                            let seg2 = seg2.trim();
                                            if let Some(bp2) = seg2.find('{') {
                                                let sig2 = seg2[..bp2].trim_end();
                                                // Only emit function signatures (end with ')')
                                                if !sig2.is_empty() && sig2.contains('(')
                                                    && sig2.trim_end().ends_with(')')
                                                {
                                                    out_lines.push(format!("{};", sig2));
                                                }
                                            }
                                            // Last segment (no '{') is the open trailing signature;
                                            // it will be connected to the '{' on the next line via Style A.
                                        }
                                    }
                                    inline_body_depth = 0;
                                    i += 1;
                                    continue;
                                }
                            }
                        }
                    }

                    // Track potential multi-line function signature start:
                    // Set sig_start when we see a col-0 line ending with '(' (param list opens).
                    if is_col0 && !lt.is_empty() && !lt.starts_with('#')
                        && !lt.starts_with("//") && !lt.starts_with("/*")
                        && !lt.starts_with("typedef") && !lt.starts_with("struct")
                        && !lt.starts_with("union") && !lt.starts_with("enum")
                        && lt.ends_with('(')
                        && sig_start_out_idx.is_none()
                        && !in_fn_body && !in_static_init {
                        // Record BEFORE this line is pushed to out_lines
                        sig_start_out_idx = Some(out_lines.len());
                        sig_start_line_idx = Some(i);
                    } else if is_col0 && !lt.is_empty() && (lt.ends_with(';') || lt.ends_with('}')) {
                        // Statement end at col 0 — clear sig tracking
                        if !in_fn_body {
                            sig_start_out_idx = None;
                            sig_start_line_idx = None;
                        }
                    }

                    // Track Style E multi-line signatures: col-0 line that starts a function
                    // signature (has `(`) but leaves the parameter list open (unbalanced parens).
                    // These are continued by indented lines; the body close "...) { ... }" may be indented.
                    if !in_fn_body && !in_static_init {
                        // Struct-returning functions: "struct foo *func_name(" — allow if contains '*'
                        let lt_is_struct_fn = (lt.starts_with("struct ") || lt.starts_with("union "))
                            && lt.contains('*') && lt.contains('(');
                        if is_col0 && lt.contains('(')
                            && !lt.starts_with('#') && !lt.starts_with("//")
                            && !lt.starts_with("typedef")
                            && (!lt.starts_with("struct") || lt_is_struct_fn)
                            && !lt.starts_with("union") && !lt.starts_with("enum")
                            && (lt.starts_with("static ") || lt.starts_with("extern ")
                                || lt.contains("inline ")
                                || lt.starts_with("int ") || lt.starts_with("long ")
                                || lt.starts_with("void ") || lt.starts_with("char ")
                                || lt.starts_with("unsigned ") || lt.starts_with("signed ")
                                || lt.starts_with("const ") || lt.starts_with("noinline ")
                                || lt.starts_with("__attribute__") || lt_is_struct_fn) {
                            // Count paren depth: if open > close, params continue on next lines
                            let paren_depth: i32 = lt.bytes().fold(0i32, |d, b| match b {
                                b'(' => d + 1,
                                b')' => d - 1,
                                _ => d,
                            });
                            if paren_depth > 0 {
                                if pending_fn_sig_start.is_none() {
                                    pending_fn_sig_start = Some(i);
                                    // Also set sig_start_out_idx if not already set
                                    if sig_start_out_idx.is_none() {
                                        sig_start_out_idx = Some(out_lines.len());
                                        sig_start_line_idx = Some(i);
                                    }
                                }
                            } else {
                                pending_fn_sig_start = None;
                            }
                        } else if is_col0 && pending_fn_sig_start.is_some()
                            && (lt.ends_with(';') || lt.ends_with('}') || lt.is_empty()) {
                            // Col-0 statement end clears pending sig
                            pending_fn_sig_start = None;
                        } else if is_fn_body_start_e {
                            // Handled above — reset after processing
                            pending_fn_sig_start = None;
                        }
                    } else if in_fn_body || in_static_init {
                        pending_fn_sig_start = None;
                    }

                    // Output this line
                    if !linemarker_re.is_match(line) {
                        out_lines.push(line.to_string());
                    }

                    i += 1;
                }

                // Write collected output to PCH file
                {
                    use std::io::Write;
                    let mut pch_out = std::io::BufWriter::new(
                        std::fs::File::create(&pch_header_path).unwrap()
                    );
                    for ol in &out_lines {
                        pch_out.write_all(ol.as_bytes()).ok();
                        pch_out.write_all(b"\n").ok();
                    }
                }

                preamble_defined_fns_outer = preamble_defined_fns;
                preamble_end_line = 1; // mark as "generated"
                eprintln!("Generated PCH header (full-file, skip fn bodies): {} ({} inline fns, {} output lines)",
                    pch_header_path, preamble_defined_fns_outer.len(), out_lines.len());
            } else {
                eprintln!("Warning: could not read {} for PCH generation", filename);
                let _ = std::fs::write(&pch_header_path, "");
            }

            // preamble_fn_names: functions defined in the preamble (inline fns from system headers)
            // For now, leave empty — all functions get delta PU files.
            // Inline functions that are re-defined in delta PUs may cause "conflicting types" errors
            // for __bswap_16 etc., but those are usually handled by the '#pragma once' or include guards.
            // The delta PU includes the pch.h which has the inline definition — and the body in the
            // delta PU defines the function again. We suppress this by adding preamble_fn_names
            // for any function found in lines[0..preamble_end_line].
            // preamble_fn_names: functions defined in the PCH preamble.
            // These should NOT get delta PU files (their bodies are already in the PCH).
            // Use preamble_defined_fns_outer (collected while scanning preamble above).
            let preamble_fn_names: FxHashSet<String> = preamble_defined_fns_outer;
            eprintln!("PCH: {} preamble fns in preamble (excluded from delta PUs)", preamble_fn_names.len());
            let (pch_path, preamble_fn_names) = (pch_header_path, preamble_fn_names);

            // Cluster-PCH mode: use dependency graph to cluster functions by shared headers.
            // Activated by PRECC_CLUSTER_PCH=1. Falls through to standard PCH if env not set.
            if std::env::var("PRECC_CLUSTER_PCH").is_ok() {
                match cluster_pch_mode(
                    filename, pu_order, &pu, &uids, &preamble_fn_names,
                    &precomputed.transitive_deps, &config,
                ) {
                    Ok((n_clusters, n_fns)) => {
                        eprintln!("cluster-PCH: done — {} clusters, {} fns", n_clusters, n_fns);
                        return; // compute_dependency returns ()
                    }
                    Err(e) => {
                        eprintln!("cluster-PCH: error {}, falling back to standard PCH", e);
                    }
                }
            }

            // Filter PUs: skip functions that are in the preamble (already in PCH)
            let pus_to_process: Vec<&String> = pu_order.iter()
                .filter(|u| {
                    if !uids.contains_key(*u) { return false; }
                    if !config.should_process_uid(*uids.get(*u).unwrap()) { return false; }
                    // Skip preamble functions — their bodies are in the PCH, not delta PUs
                    if let Some(fname) = extract_key_name(u) {
                        if preamble_fn_names.contains(fname) {
                            // Also remove any stale PU file from a previous run
                            if let Some(uid_val) = uids.get(*u) {
                                let a: Vec<&str> = u.split(':').collect();
                                if a.len() >= 3 {
                                    let file_str = a[2];
                                    let stale = if *uid_val == 0 {
                                        format!("{}.pu.c", file_str)
                                    } else {
                                        format!("{}_{}.pu.c", file_str, uid_val)
                                    };
                                    let _ = std::fs::remove_file(&stale);
                                }
                            }
                            return false;
                        }
                    }
                    true
                })
                .collect();
            // Bundle mode: group all delta function bodies into N bundle files.
            // Each bundle: #include "pch.h" + concatenated function bodies.
            // With N bundles compiled in parallel with -jN, this is much faster
            // than N*2496 individual GCC invocations each re-loading the .gch.
            let n_bundles: usize = std::env::var("PRECC_PCH_BUNDLES")
                .ok().and_then(|s| s.parse().ok()).unwrap_or(8);
            let pch_include = if pch_path.is_empty() {
                String::new()
            } else {
                let pch_basename = std::path::Path::new(&pch_path)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| pch_path.clone());
                format!("#include \"{}\"\n", pch_basename)
            };
            // Determine base filename (e.g. "sqlite3.i") for bundle naming
            let base_name = std::path::Path::new(&pch_path)
                .file_stem()  // "sqlite3.i.pch"
                .and_then(|s| s.to_str())
                .and_then(|s| s.strip_suffix(".pch"))  // "sqlite3.i"
                .unwrap_or("bundle")
                .to_string();
            // Build list of (output_file, body) pairs in order
            let delta_items: Vec<(String, String)> = pus_to_process.iter().map(|u| {
                let uid = *uids.get(*u).unwrap();
                let a: Vec<&str> = u.split(':').collect();
                let file_str = if a.len() >= 3 { a[2] } else { "" };
                let output_file = if uid == 0 {
                    format!("{}.pu.c", file_str)
                } else {
                    format!("{}_{}.pu.c", file_str, uid)
                };
                let body = pu.get(u.as_str()).map(|s| s.clone()).unwrap_or_default();
                (output_file, body)
            }).collect();
            // Remove individual delta PU files from previous runs (they'd conflict with bundles)
            for (path, _) in &delta_items {
                let _ = std::fs::remove_file(path);
            }
            // Write bundle files
            let n_bundles = n_bundles.min(delta_items.len()).max(1);
            let per_bundle = (delta_items.len() + n_bundles - 1) / n_bundles;
            let mut bundle_files: Vec<String> = Vec::new();
            for b in 0..n_bundles {
                let start = b * per_bundle;
                if start >= delta_items.len() { break; }
                let end = (start + per_bundle).min(delta_items.len());
                let bundle_name = format!("{}.bundle_{}.pu.c", base_name, b);
                let mut content = pch_include.clone();
                for (_, body) in &delta_items[start..end] {
                    content.push_str(body);
                    content.push('\n');
                }
                let _ = std::fs::write(&bundle_name, &content);
                bundle_files.push(bundle_name);
            }
            eprintln!("PCH: wrote {} bundle files ({} fns total, {} per bundle)",
                bundle_files.len(), delta_items.len(), per_bundle);

        } else {
            // Standard split mode: each function gets its own file
            // OPTIMIZATION: Count PUs to process and choose sequential vs parallel
            let pus_to_process: Vec<&String> = pu_order.iter()
                .filter(|u| uids.contains_key(*u) && config.should_process_uid(*uids.get(*u).unwrap()))
                .collect();

            // Parallel processing: each PU writes to a unique output file (no shared mutable state).
            // All inputs are read-only — safe to parallelise. Thread count controlled by
            // PRECC_PU_THREADS (default 4) to avoid over-subscription under xargs -Pn.
            // Only build a thread pool for large files (>=8 PUs); small files run sequentially
            // to avoid the ~300ms ThreadPoolBuilder overhead.
            let pu_threads: usize = std::env::var("PRECC_PU_THREADS")
                .ok().and_then(|s| s.parse().ok()).unwrap_or(4);
            let use_par = pu_threads > 1 && pus_to_process.len() >= 8;
            let maybe_pool = if use_par {
                rayon::ThreadPoolBuilder::new()
                    .num_threads(pu_threads)
                    .build()
                    .ok()
            } else {
                None
            };
            let run_pus = |f: &(dyn Fn(&String) + Sync)| {
                if let Some(ref pool) = maybe_pool {
                    pool.install(|| pus_to_process.par_iter().for_each(|u| f(*u)));
                } else {
                    pus_to_process.iter().for_each(|u| f(*u));
                }
            };
            run_pus(&|u| {
                let uid = *uids.get(u).unwrap();
                let j = *pids.get(u).unwrap();
                let mut necessary: FxHashSet<String> = Default::default();
                necessary.insert(u.to_string());
                use_dependency(uid,
                    &mut necessary,
                    &pu,
                    &pu_order[0..j+1],
                    j,
                    &precomputed.position_index,
                    true,
                    common_header.as_deref(),
                    &common_deps,
                    system_typedefs,
                    None,
                    &extern_functions,
                    &extern_variables,
                    &static_funcptr_vars,
                    &precomputed.shared_maps,
                    &precomputed.transitive_deps,
                    &tags,
                    &precomputed.project_types,
                    &precomputed.code_identifiers,
                    &precomputed.interner,
                    &precomputed.interned_trans_deps,
                    &precomputed.interned_pos_index,
                );
            });

        }
    } else {
        // Non-split mode: single output file
        let max_pos = pu_order.len().saturating_sub(1);
        let mut necessary: FxHashSet<String> = Default::default();
        for u in pu_order.iter() {
            let type_str = u.split(':').next().unwrap_or("");
            let pu_type = PuType::from_str(type_str);
            // Include: functions (main code), variables (including externs), typedefs
            // Exclude: enumerators (handled via parent enum), aliases (handled separately)
            if pu_type.is_nosplit_tracked() {
                necessary.insert(u.to_string());
            }
        }
        use_dependency(0,
            &mut necessary,
            &pu,
            &pu_order,
            max_pos,
            &precomputed.position_index,
            false,  // is_split_mode
            None,
            &FxHashSet::default(),
            &[],
            None,  // no primary_functions in non-split mode
            &extern_functions,
            &FxHashMap::default(),
            &static_funcptr_vars,
            &precomputed.shared_maps,
            &precomputed.transitive_deps,
            &tags,
            &precomputed.project_types,
            &precomputed.code_identifiers,
            &precomputed.interner,
            &precomputed.interned_trans_deps,
            &precomputed.interned_pos_index,
        );
    }
}

/// Unified function for processing dependencies and generating output
/// Handles both standard split mode and chunked mode via the optional primary_functions parameter
/// - primary_functions: None for standard mode, Some(&set) for chunked mode (functions to keep full bodies)
#[allow(dead_code)]
#[inline(always)]
fn use_dependency(
    uid: usize,
    necessary: &mut FxHashSet<String>,
    pu: &FxHashMap<String, String>,
    pu_order: &[String],
    max_pos: usize,  // Maximum position for valid_keys check (j in caller)
    position_index: &PositionIndex,  // Pre-computed position index for O(1) valid_keys checks
    is_split_mode: bool,
    common_header: Option<&str>,
    common_deps: &FxHashSet<String>,
    system_typedefs: &[(String, String)],
    primary_functions: Option<&FxHashSet<String>>,  // For chunked mode: functions to keep full body
    extern_functions: &FxHashMap<String, String>,
    extern_variables: &FxHashMap<String, String>,  // Bug48: extern const struct declarations
    static_funcptr_vars: &FxHashMap<String, String>,  // Bug71: static function pointer variables
    shared_maps: &SharedMaps,  // Pre-computed shared maps
    _transitive_deps: &TransitiveDeps,  // Pre-computed transitive dependencies
    tags: &FxHashMap<String, Vec<String>>,
    project_types: &ProjectTypes,  // Pre-computed project types
    code_identifiers: &CodeIdentifiers,  // Pre-tokenized identifiers (Optimization #1)
    // Interned structures for fast InternId-based operations
    interner: &GlobalInterner,
    interned_trans_deps: &InternedTransitiveDeps,
    interned_pos_index: &InternedPositionIndex,
) -> usize {
    USE_DEP_COUNT.fetch_add(1, Ordering::Relaxed);
    let t0 = Instant::now();

    // OPTIMIZATION: Use interned IDs for fast transitive dependency resolution
    // The initial loop converts strings to InternIds, resolves deps via InternId,
    // and filters by position using InternId - all avoiding string hashing/comparison

    // Save initial necessary items (we need them for the fixpoint loop)
    let initial_necessary: Vec<String> = necessary.iter().cloned().collect();

    // First pass: resolve transitive deps using interned structures
    for u in initial_necessary.iter() {
        if let Some(key_id) = interner.get_id(u) {
            if let Some(deps) = interned_trans_deps.get(key_id) {
                // Filter by position using InternId and add to necessary
                for &dep_id in deps.iter() {
                    if interned_pos_index.is_valid(dep_id, max_pos) {
                        let dep_str = interner.get_str(dep_id);
                        necessary.insert(dep_str.to_string());
                    }
                }
            }
        }
    }
    let t1 = Instant::now();
    USE_DEP_TRANS_NS.fetch_add((t1 - t0).as_nanos() as u64, Ordering::Relaxed);

    // Post-process: scan code content for prototype references not captured by ctags
    // Uses pre-computed shared_maps instead of rebuilding maps each time
    scan_for_prototype_references_optimized(necessary, pu, shared_maps);

    // Bug-frag/st_pop fix: Promote raw function keys to full function keys when the
    // function is defined WITHIN the current pu_order slice (i.e., position <= max_pos).
    // Raw keys like "function:/tmp/regexp.i" are intentionally used so that functions
    // referenced "after" the current function don't get included as full bodies.
    // But for functions defined BEFORE the current function (earlier in source order),
    // we want their full body to be output so the return type is correct.
    // Without this, "static Frag_T st_pop()" gets no forward declaration AND no body output,
    // causing "incompatible types when assigning to type 'Frag_T' from type 'int'" errors.
    if is_split_mode {
        let raw_func_keys: Vec<String> = necessary.iter()
            .filter(|u| {
                // Raw function keys: "function:file" (no name embedded)
                // Full function keys: "function:name:file" (name embedded, has 3 parts)
                if let Some(rest) = u.strip_prefix("function:") {
                    !rest.contains(':')  // No second colon = raw key
                } else {
                    false
                }
            })
            .cloned()
            .collect();
        if !raw_func_keys.is_empty() {
            let mut to_promote: Vec<(String, String)> = Vec::new();  // (raw_key, full_key)
            // Find all function names in tags that map to these raw keys
            for (func_name, units) in tags.iter() {
                for unit in units.iter() {
                    if PuType::from_key(unit) == PuType::Function {
                        // unit format: "function:file"
                        if raw_func_keys.contains(unit) {
                            if let Some((_, file_part)) = parse_key_type_rest(unit) {
                                let full_key = format!("function:{}:{}", func_name, file_part);
                                // Only promote if the full key is in pu AND within the slice
                                if pu.contains_key(&full_key) {
                                    if let Some(pos) = position_index.get_pos(&full_key) {
                                        if pos <= max_pos {
                                            to_promote.push((unit.clone(), full_key));
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            for (raw_key, full_key) in to_promote {
                necessary.remove(&raw_key);
                necessary.insert(full_key);
            }
        }
    }

    let t2 = Instant::now();
    USE_DEP_PROTO_SCAN_NS.fetch_add((t2 - t1).as_nanos() as u64, Ordering::Relaxed);

    // Post-process: scan code content for typedef references not captured by ctags (struct member types)
    // Bug70 fix: Also pass tags to resolve struct aliases (inline struct definitions aliased to variables)
    if std::env::var("DEBUG_TYPEDEF_SCAN").is_ok() {
        let has_cleanup = necessary.contains("struct:cleanup_stuff:/tmp/regexp.i");
        let has_except_T = necessary.contains("typedef:except_T:/tmp/regexp.i");
        let has_vim_ex = necessary.contains("struct:vim_exception:/tmp/regexp.i");
        let has_except = necessary.contains("typedef:except_type_T:/tmp/regexp.i");
        if has_cleanup || has_except_T || has_vim_ex || has_except {
            eprintln!("DEBUG_TYPEDEF_SCAN before: cleanup_stuff={} except_T={} vim_exception={} except_type_T={} necessary.len={}", has_cleanup, has_except_T, has_vim_ex, has_except, necessary.len());
        }
    }
    scan_for_typedef_references_optimized(necessary, pu, shared_maps, pu_order, tags);
    if std::env::var("DEBUG_TYPEDEF_SCAN").is_ok() {
        let has_cleanup = necessary.contains("struct:cleanup_stuff:/tmp/regexp.i");
        let has_except_T = necessary.contains("typedef:except_T:/tmp/regexp.i");
        let has_vim_ex = necessary.contains("struct:vim_exception:/tmp/regexp.i");
        let has_except = necessary.contains("typedef:except_type_T:/tmp/regexp.i");
        if has_cleanup || has_except_T || has_vim_ex || has_except {
            eprintln!("DEBUG_TYPEDEF_SCAN after: cleanup_stuff={} except_T={} vim_exception={} except_type_T={}", has_cleanup, has_except_T, has_vim_ex, has_except);
        }
    }
    let t3 = Instant::now();
    USE_DEP_TYPEDEF_SCAN_NS.fetch_add((t3 - t2).as_nanos() as u64, Ordering::Relaxed);

    // Bug36 fix: Resolve typedef dependencies for referenced extern variables.
    // When code references an extern variable (e.g., x_jump_env : jmp_buf),
    // the scan above only looks at code tokens — it sees "x_jump_env" but not "jmp_buf".
    // We must add the variable's type typedef to necessary explicitly.
    if is_split_mode && !extern_variables.is_empty() {
        let all_code_ids: FxHashSet<&str> = code_identifiers.get_union(necessary.iter());
        let mut new_typedef_keys: Vec<String> = Vec::new();
        for (var_name, decl) in extern_variables.iter() {
            if !all_code_ids.contains(var_name.as_str()) {
                continue;
            }
            // Extract the type identifier from "extern [qualifiers] TYPE VAR[...];"
            // We do NOT filter out jmp_buf/sigjmp_buf here — they may be project typedefs.
            let trimmed = decl.trim();
            if let Some(rest) = trimmed.strip_prefix("extern") {
                let rest = rest.trim();
                // Skip qualifiers/struct/union/enum
                let mut words: Vec<&str> = rest.split_whitespace().collect();
                while !words.is_empty() && ["const", "volatile", "register"].contains(&words[0]) {
                    words.remove(0);
                }
                if words.len() >= 2 && !["struct", "union", "enum"].contains(&words[0]) {
                    let type_name = words[0].trim_end_matches('*');
                    if shared_maps.all_typedef_names.contains(type_name) {
                        if let Some(typedef_units) = shared_maps.typedef_map.get(type_name) {
                            for typedef_unit in typedef_units {
                                if !necessary.contains(typedef_unit) {
                                    new_typedef_keys.push(typedef_unit.clone());
                                }
                            }
                        }
                    }
                }
            }
        }
        for key in new_typedef_keys {
            necessary.insert(key);
        }
    }

    // Bug40 fix: After adding new typedefs via scan_for_typedef_references,
    // resolve their transitive dependencies from the newly added units
    // OPTIMIZATION: Use interned fixpoint loop for fast InternId-based operations
    // Bug-bufstate/except_type fix: Iterate scan+fixpoint until convergence:
    // fixpoint_transitive_deps_interned may add new structs (e.g. struct:vim_exception)
    // whose bodies reference new typedefs (e.g. except_type_T) not yet scanned.
    loop {
        let size_before = necessary.len();
        fixpoint_transitive_deps_interned(
            necessary,
            &initial_necessary,
            max_pos,
            interner,
            interned_trans_deps,
            interned_pos_index,
        );
        let size_after_fixpoint = necessary.len();
        if size_after_fixpoint > size_before {
            // New items added by fixpoint — rescan for typedef references in new items
            scan_for_typedef_references_optimized(necessary, pu, shared_maps, pu_order, tags);
        }
        if necessary.len() == size_after_fixpoint {
            break;  // Converged
        }
    }
    let t4 = Instant::now();
    USE_DEP_FIXPOINT_NS.fetch_add((t4 - t3).as_nanos() as u64, Ordering::Relaxed);

    // Call unified print function with optional primary_functions
    // Bug68 fix: Pass target function (initial_necessary[0]) for accurate is_primary check
    let target_function = initial_necessary.first().map(|s| s.as_str());
    print_necessary_units(pu_order, necessary, pu, tags, uid, is_split_mode, common_header, common_deps, system_typedefs, primary_functions, extern_functions, extern_variables, static_funcptr_vars, project_types, shared_maps, code_identifiers, position_index, target_function);
    let t5 = Instant::now();
    USE_DEP_PRINT_NS.fetch_add((t5 - t4).as_nanos() as u64, Ordering::Relaxed);

    uid + 1
}

/// Scan code content of necessary units for prototype references not captured by ctags.
/// This handles array initializers and other cases where ctags doesn't emit depends_on().
/// NOTE: Non-optimized version - use scan_for_prototype_references_optimized instead.
#[allow(dead_code)]
#[inline(always)]
fn scan_for_prototype_references(
    necessary: &mut FxHashSet<String>,
    pu: &FxHashMap<String, String>,
    tags: &FxHashMap<String, Vec<String>>,
) {
    // Build maps of names -> unit keys for both prototypes and functions
    let mut prototype_map: FxHashMap<&str, Vec<&String>> = FxHashMap::default();
    let mut function_map: FxHashMap<&str, Vec<&String>> = FxHashMap::default();

    for (name, units) in tags.iter() {
        for unit in units.iter() {
            // Use PuType::from_key for O(1) first-byte dispatch
            match PuType::from_key(unit) {
                PuType::Prototype => {
                    prototype_map.entry(name.as_str()).or_default().push(unit);
                }
                PuType::Function => {
                    function_map.entry(name.as_str()).or_default().push(unit);
                }
                _ => {}
            }
        }
    }

    // Combine all function names we want to search for
    let all_names: FxHashSet<&str> = prototype_map.keys()
        .chain(function_map.keys())
        .copied()
        .collect();

    if all_names.is_empty() {
        return;
    }

    // Use fast tokenizer instead of regex for better performance
    // Scan all necessary units' code for function/prototype references
    let mut to_add: FxHashSet<String> = FxHashSet::default();

    // Helper closure to scan code and collect function references
    // Uses fast tokenizer instead of regex
    let scan_code = |code: &str, already_found: &FxHashSet<String>, new_found: &mut Vec<String>| {
        for matched_name in tokenize_c_identifiers(code) {

            // Skip if not a known function/prototype name
            if !all_names.contains(matched_name) {
                continue;
            }

            // First, try to find a prototype for this function
            if let Some(proto_units) = prototype_map.get(matched_name) {
                for proto_unit in proto_units {
                    if let Some(file_part) = proto_unit.strip_prefix("prototype:") {
                        let full_key = format!("prototype:{}:{}", matched_name, file_part);
                        if !necessary.contains(&full_key) && !already_found.contains(&full_key) {
                            new_found.push(full_key);
                        }
                    }
                }
            }

            // If no prototype found, check if there's a function definition
            // The function will be converted to a forward declaration during output
            if prototype_map.get(matched_name).is_none() {
                if let Some(func_units) = function_map.get(matched_name) {
                    for func_unit in func_units {
                        // Try to build the full key "function:name:file" from raw "function:file"
                        // If the full key is in pu, use it (so pass3 outputs the full body).
                        // Otherwise, use the raw key (which generates a K&R forward declaration).
                        let key_to_add = if let Some(file_part) = func_unit.strip_prefix("function:") {
                            let full_key = format!("function:{}:{}", matched_name, file_part);
                            if pu.contains_key(&full_key) {
                                full_key
                            } else {
                                (*func_unit).clone()
                            }
                        } else {
                            (*func_unit).clone()
                        };
                        if !necessary.contains(&key_to_add) && !already_found.contains(&key_to_add) {
                            new_found.push(key_to_add);
                        }
                    }
                }
            }
        }
    };

    // First pass: scan necessary units
    let mut new_found: Vec<String> = Vec::new();
    for unit_key in necessary.iter() {
        if let Some(code) = pu.get(unit_key) {
            scan_code(code, &to_add, &mut new_found);
        }
    }
    for key in new_found.drain(..) {
        to_add.insert(key);
    }

    // Iteratively scan newly added units until no more found
    loop {
        let to_add_snapshot: Vec<String> = to_add.iter().cloned().collect();
        let mut found_new = false;

        for unit_key in to_add_snapshot.iter() {
            if let Some(code) = pu.get(unit_key) {
                scan_code(code, &to_add, &mut new_found);
            }
        }

        for key in new_found.drain(..) {
            if to_add.insert(key) {
                found_new = true;
            }
        }

        if !found_new {
            break;
        }
    }

    // Add all found units to necessary
    necessary.extend(to_add);
}

/// Scan code for typedef references that ctags didn't capture
/// This is needed because struct member types are not captured by ctags (-m option)
/// NOTE: Non-optimized version - use scan_for_typedef_references_optimized instead.
#[allow(dead_code)]
fn scan_for_typedef_references(
    necessary: &mut FxHashSet<String>,
    pu: &FxHashMap<String, String>,
    tags: &FxHashMap<String, Vec<String>>,
    pu_order: &[String],  // Also scan units in the current slice
) {
    // Build a map of typedef names -> unit keys
    // The `tags` map stores: name -> Vec<full_unit_key>
    // where full_unit_key is like "typedef:sqlite3StatValueType:sqlite3.i:7123"
    let mut typedef_map: FxHashMap<&str, Vec<String>> = FxHashMap::default();

    for (name, units) in tags.iter() {
        for unit in units.iter() {
            if PuType::from_key(unit) == PuType::Typedef {
                // Unit format is "typedef:filename" (e.g., "typedef:sqlite3.i")
                // Pu key format is "typedef:name:filename" (e.g., "typedef:sqlite3StatValueType:sqlite3.i")
                // Extract the filename from the unit - use efficient parser
                if let Some((_, filename)) = parse_key_type_rest(unit) {
                    let pu_key = format!("typedef:{}:{}", name, filename);
                    typedef_map.entry(name.as_str()).or_default().push(pu_key);
                }
            }
        }
    }

    if typedef_map.is_empty() {
        return;
    }

    // Collect all typedef names for quick lookup
    let all_typedef_names: FxHashSet<&str> = typedef_map.keys().copied().collect();

    let mut to_add: FxHashSet<String> = FxHashSet::default();

    // Scan code for typedef references using fast tokenizer
    let scan_code = |code: &str, already_found: &FxHashSet<String>, new_found: &mut Vec<String>, necessary: &FxHashSet<String>| {
        for matched_name in tokenize_c_identifiers(code) {

            // Skip if not a known typedef name
            if !all_typedef_names.contains(matched_name) {
                continue;
            }

            if let Some(typedef_units) = typedef_map.get(matched_name) {
                for typedef_unit in typedef_units {
                    if !necessary.contains(typedef_unit) && !already_found.contains(typedef_unit) {
                        new_found.push(typedef_unit.clone());
                    }
                }
            }
        }
    };

    // First pass: scan both necessary units AND pu_order (current slice) for typedef references
    // This catches typedefs used in struct fields that ctags doesn't track as dependencies
    let mut new_found: Vec<String> = Vec::new();

    // Scan necessary units
    for unit_key in necessary.iter() {
        if let Some(code) = pu.get(unit_key) {
            scan_code(code, &to_add, &mut new_found, necessary);
        }
    }

    // Also scan pu_order units (the current slice being output)
    for unit_key in pu_order.iter() {
        if let Some(code) = pu.get(unit_key) {
            scan_code(code, &to_add, &mut new_found, necessary);
        }
    }

    for key in new_found.drain(..) {
        to_add.insert(key);
    }

    // Iteratively scan newly added units until no more found
    loop {
        let to_add_snapshot: Vec<String> = to_add.iter().cloned().collect();
        let mut found_new = false;

        for unit_key in to_add_snapshot.iter() {
            if let Some(code) = pu.get(unit_key) {
                scan_code(code, &to_add, &mut new_found, necessary);
            }
        }

        for key in new_found.drain(..) {
            if to_add.insert(key) {
                found_new = true;
            }
        }

        if !found_new {
            break;
        }
    }

    // Add all found typedef units to necessary
    necessary.extend(to_add);
}

/// Optimized version of scan_for_prototype_references that uses pre-computed SharedMaps
/// This avoids rebuilding prototype_map and function_map for each of 2485 PUs
/// Uses fast tokenizer instead of regex for ~10x speedup
#[inline(always)]
fn scan_for_prototype_references_optimized(
    necessary: &mut FxHashSet<String>,
    pu: &FxHashMap<String, String>,
    shared_maps: &SharedMaps,
) {
    // Use pre-computed maps instead of rebuilding them
    if shared_maps.all_func_names.is_empty() {
        return;
    }

    // Scan all necessary units' code for function/prototype references
    let mut to_add: FxHashSet<String> = FxHashSet::default();

    // Helper closure to scan code and collect function references
    // Uses fast tokenizer instead of regex
    // OPTIMIZATION: SharedMaps now stores full pu_keys - use directly (no format!() needed)
    let scan_code = |code: &str, already_found: &FxHashSet<String>, new_found: &mut Vec<String>, necessary: &FxHashSet<String>| {
        // Extract identifiers from code once (O(n)) instead of regex matching
        for matched_name in tokenize_c_identifiers(code) {
            // Skip if not a known function/prototype name
            if !shared_maps.all_func_names.contains(matched_name) {
                continue;
            }

            // First, try to find a prototype for this function
            // SharedMaps now stores full pu_keys directly (format: "prototype:name:file")
            if let Some(proto_units) = shared_maps.prototype_map.get(matched_name) {
                for full_key in proto_units {
                    if !necessary.contains(full_key) && !already_found.contains(full_key) {
                        new_found.push(full_key.clone());
                    }
                }
            }

            // If no prototype found, check if there's a function definition
            // The function will be converted to a forward declaration during output
            if shared_maps.prototype_map.get(matched_name).is_none() {
                if let Some(func_units) = shared_maps.function_map.get(matched_name) {
                    for func_unit in func_units {
                        if !necessary.contains(func_unit) && !already_found.contains(func_unit) {
                            new_found.push(func_unit.clone());
                        }
                    }
                }
            }
        }
    };

    // First pass: scan necessary units
    let mut new_found: Vec<String> = Vec::new();
    for unit_key in necessary.iter() {
        if let Some(code) = pu.get(unit_key) {
            scan_code(code, &to_add, &mut new_found, necessary);
        }
    }
    for key in new_found.drain(..) {
        to_add.insert(key);
    }

    // OPTIMIZATION: Track processed keys to avoid cloning entire to_add set each iteration
    // Use a "frontier" approach: process only newly added items
    let mut processed: FxHashSet<String> = FxHashSet::default();

    // Iteratively scan newly added units until no more found
    loop {
        // Get keys to process (in to_add but not yet processed) - avoid cloning by collecting refs
        let to_process: Vec<&String> = to_add.iter()
            .filter(|k| !processed.contains(*k))
            .collect();

        if to_process.is_empty() {
            break;
        }

        for unit_key in to_process {
            processed.insert(unit_key.clone());  // Mark as processed
            if let Some(code) = pu.get(unit_key) {
                scan_code(code, &to_add, &mut new_found, necessary);
            }
        }

        // Insert new found items
        for key in new_found.drain(..) {
            to_add.insert(key);
        }
    }

    // Add all found units to necessary
    necessary.extend(to_add);
}

/// Optimized version of scan_for_typedef_references that uses pre-computed SharedMaps
/// This avoids rebuilding typedef_map for each of 2485 PUs
/// Uses fast tokenizer instead of regex for ~10x speedup
/// Bug36 fix: Also scans for struct/union references in typedefs
/// Bug60 fix: Also scans for enumerator references and adds their parent enums
/// Bug70 fix: Also checks tags for struct aliases (inline struct definitions aliased to variables)
fn scan_for_typedef_references_optimized(
    necessary: &mut FxHashSet<String>,
    pu: &FxHashMap<String, String>,
    shared_maps: &SharedMaps,
    pu_order: &[String],
    tags: &FxHashMap<String, Vec<String>>,
) {
    // Use pre-computed maps instead of rebuilding them
    // Bug60 fix: Also check for enumerator names
    if shared_maps.all_typedef_names.is_empty()
        && shared_maps.all_struct_names.is_empty()
        && shared_maps.all_union_names.is_empty()
        && shared_maps.all_enumerator_names.is_empty()
        && shared_maps.all_variable_names.is_empty() {
        return;
    }

    let mut to_add: FxHashSet<String> = FxHashSet::default();

    // Scan code for typedef references using fast tokenizer
    let scan_code = |code: &str, already_found: &FxHashSet<String>, new_found: &mut Vec<String>, necessary: &FxHashSet<String>| {
        // Track if we just saw "struct" or "union" keyword
        let mut prev_was_struct = false;
        let mut prev_was_union = false;

        for matched_name in tokenize_c_identifiers(code) {
            // Bug36: Check for struct/union name after "struct"/"union" keyword
            // Bug70: Also check tags for struct aliases (inline struct definitions aliased to variables)
            if prev_was_struct {
                prev_was_struct = false;
                let mut found_in_struct_map = false;
                if shared_maps.all_struct_names.contains(matched_name) {
                    if let Some(struct_units) = shared_maps.struct_map.get(matched_name) {
                        for struct_unit in struct_units {
                            if !necessary.contains(struct_unit) && !already_found.contains(struct_unit) {
                                new_found.push(struct_unit.clone());
                                found_in_struct_map = true;
                            }
                        }
                    }
                }
                // Bug70 fix: If not found in struct_map, check tags for struct aliases
                // These are inline struct definitions that ctags aliases to variables or typedefs
                // Example 1: static struct key_name_entry { ... } key_names_table[] = {...};
                //   ctags creates: key_name_entry -> variable:key_names_table:file
                // Example 2: typedef struct subs_expr_S { ... } subs_expr_T;
                //   ctags creates: subs_expr_S -> typedef:subs_expr_T:file
                let mut found_in_tags = false;
                if !found_in_struct_map {
                    if let Some(alias_units) = tags.get(matched_name) {
                        for alias_unit in alias_units {
                            // Add variable/externvar/typedef aliases (these contain the struct definition)
                            let unit_type = PuType::from_key(alias_unit);
                            if unit_type == PuType::Variable || unit_type == PuType::ExternVar || unit_type == PuType::Typedef {
                                if !necessary.contains(alias_unit) && !already_found.contains(alias_unit) {
                                    new_found.push(alias_unit.clone());
                                    found_in_tags = true;
                                }
                            }
                        }
                    }
                }
                if found_in_struct_map || found_in_tags {
                    continue;  // Token was handled as a struct/alias name
                }
                // If not found as struct or alias, fall through to check as typedef/enum/variable
            }
            if prev_was_union {
                prev_was_union = false;
                if shared_maps.all_union_names.contains(matched_name) {
                    if let Some(union_units) = shared_maps.union_map.get(matched_name) {
                        for union_unit in union_units {
                            if !necessary.contains(union_unit) && !already_found.contains(union_unit) {
                                new_found.push(union_unit.clone());
                            }
                        }
                    }
                    continue;  // Only skip further checks if this token IS a union name
                }
                // If not a union name, fall through to check as typedef/enum/variable
            }

            // Check for struct/union keywords
            if matched_name == "struct" {
                prev_was_struct = true;
                continue;
            }
            if matched_name == "union" {
                prev_was_union = true;
                continue;
            }

            // Check for typedef names
            if shared_maps.all_typedef_names.contains(matched_name) {
                if let Some(typedef_units) = shared_maps.typedef_map.get(matched_name) {
                    for typedef_unit in typedef_units {
                        if !necessary.contains(typedef_unit) && !already_found.contains(typedef_unit) {
                            new_found.push(typedef_unit.clone());
                        }
                    }
                }
                continue;
            }

            // Bug60 fix: Check for enumerator names (e.g., KS_XON)
            // When an enumerator is used, we need to include its parent enum
            if shared_maps.all_enumerator_names.contains(matched_name) {
                if let Some(parent_enum_key) = shared_maps.enumerator_map.get(matched_name) {
                    if !necessary.contains(parent_enum_key) && !already_found.contains(parent_enum_key) {
                        new_found.push(parent_enum_key.clone());
                    }
                }
            }

            // Check for file-scope static variable names (e.g., rex, regcode, nstate)
            // These are not types, so they're not caught by the typedef/struct/enum checks.
            // A function that references a global variable needs it declared.
            if shared_maps.all_variable_names.contains(matched_name) {
                if let Some(var_units) = shared_maps.variable_map.get(matched_name) {
                    for var_unit in var_units {
                        if !necessary.contains(var_unit) && !already_found.contains(var_unit) {
                            new_found.push(var_unit.clone());
                        }
                    }
                }
            }
        }
    };

    // First pass: scan necessary units for typedef references
    // NOTE: Removed O(n²) pu_order scan - with improved ctags (Bug57 fix),
    // struct dependencies are now properly captured via depends_on callbacks
    let mut new_found: Vec<String> = Vec::new();

    // Scan necessary units only (not the entire pu_order slice - was O(n²))
    for unit_key in necessary.iter() {
        if let Some(code) = pu.get(unit_key) {
            scan_code(code, &to_add, &mut new_found, necessary);
        }
    }

    // Suppress unused warning for pu_order - kept as parameter for API compatibility
    let _ = pu_order;

    for key in new_found.drain(..) {
        to_add.insert(key);
    }

    // OPTIMIZATION: Track processed keys to avoid cloning entire to_add set each iteration
    // Use a "frontier" approach: process only newly added items
    let mut processed: FxHashSet<String> = FxHashSet::default();

    // Iteratively scan newly added units until no more found
    loop {
        // Get keys to process (in to_add but not yet processed) - avoid cloning by collecting refs
        let to_process: Vec<&String> = to_add.iter()
            .filter(|k| !processed.contains(*k))
            .collect();

        if to_process.is_empty() {
            break;
        }

        for unit_key in to_process {
            processed.insert(unit_key.clone());  // Mark as processed
            if let Some(code) = pu.get(unit_key) {
                scan_code(code, &to_add, &mut new_found, necessary);
            }
        }

        // Insert new found items
        for key in new_found.drain(..) {
            to_add.insert(key);
        }
    }

    // Add all found typedef/struct/union/enum units to necessary
    // Bug60 fix: Now includes parent enums for referenced enumerators
    necessary.extend(to_add);
}

#[allow(dead_code)]
#[inline(always)]
fn update_dependency(
    from: String,
    c: usize,
    necessary: &mut FxHashSet<String>,
    get: &mut Vec<String>,
    tags: &FxHashMap<String, Vec<String>>,
) -> usize {
    let mut c = c;
    let mut removed_names = FxHashSet::<String>::with_capacity_and_hasher(128, Default::default());
    for to in get.iter() {
        /*
        if to == "wchar_t" {
             eprintln!("Checking dependency: wchar_t");
             if let Some(u) = tags.get(to) {
                 eprintln!("Found in tags: {}", u);
             } else {
                 eprintln!("NOT found in tags");
             }
        }
        */
	if to.to_string() != from.to_string() {
		if let Some(units) = tags.get(to) {
		    for u_val in units {
			let (type_str, file_str) = {
			    let parts: Vec<&str> = u_val.split(":").collect();
			    (parts[0], parts[1])
			};
			let u = format!("{}:{}:{}", type_str, to, file_str);
			if !necessary.contains(&u) {
			    removed_names.insert(to.to_string());
			    necessary.insert(u);
			    c = c + 1;
			}
		    }
		}
       }
    }
    for name in removed_names.iter() {
	let index = get.iter().position(|x| x == name).unwrap();
	get.remove(index);
    }
    c
}

#[allow(dead_code)]
#[inline(always)]
fn update_dependency_optimized(
    from: &str,
    mut c: usize,
    necessary: &mut FxHashSet<String>,
    deps: &[String],
    tags: &FxHashMap<String, Vec<String>>,
    processed: &mut FxHashSet<String>,
    _dep: &FxHashMap<String, Vec<String>>,
    enumerator_to_enum: &FxHashMap<String, String>,  // Maps enumerator name to parent enum unit
) -> usize {
    // OPTIMIZATION: Pre-allocate buffer for building keys, reuse across iterations
    let mut key_buf = String::with_capacity(128);

    for to in deps.iter() {
	// Skip if already processed or self-reference
	if processed.contains(to) || to == from {
	    continue;
	}

	if let Some(units) = tags.get(to) {
	    for u_val in units {
		// Check if this is a 3-part alias format (type:name:file) or 2-part (type:file)
		let parts: Vec<&str> = u_val.splitn(3, ':').collect();

		// OPTIMIZATION: Use buffer instead of format!()
		key_buf.clear();
		if parts.len() == 3 {
		    // Alias format: type:name:file - use name from the tag entry, not from lookup
		    key_buf.push_str(parts[0]);
		    key_buf.push(':');
		    key_buf.push_str(parts[1]);
		    key_buf.push(':');
		    key_buf.push_str(parts[2]);
		} else if parts.len() == 2 {
		    // Standard format: type:file - use lookup key as name
		    key_buf.push_str(parts[0]);
		    key_buf.push(':');
		    key_buf.push_str(to);
		    key_buf.push(':');
		    key_buf.push_str(parts[1]);
		} else {
		    continue;
		}

		if !necessary.contains(&key_buf) {
		    // Check enumerator first before moving key_buf into HashSet
		    let pu_type = PuType::from_str(parts[0]);
		    if pu_type == PuType::Enumerator {
			// Use the enumerator_to_enum map to find the parent enum
			if let Some(parent_enum_u) = enumerator_to_enum.get(to) {
			    if !necessary.contains(parent_enum_u) {
				necessary.insert(parent_enum_u.clone());
				c += 1;
			    }
			}
		    }
		    // OPTIMIZATION: Clone only once when inserting (not format + clone)
		    necessary.insert(key_buf.clone());
		    c += 1;
		}
	    }
	    // Mark as processed
	    processed.insert(to.clone());
	}
    }
    c
}

/// Unified function for generating PU output files
/// Handles both standard split mode and chunked mode via the optional primary_functions parameter
/// - primary_functions: None for standard mode (all functions get full bodies)
///                      Some(&set) for chunked mode (only primary functions get full bodies)
#[allow(dead_code)]
#[inline(always)]
fn print_necessary_units(
    pu_order: &[String],
    necessary: &FxHashSet<String>,
    pu: &FxHashMap<String, String>,
    tags: &FxHashMap<String, Vec<String>>,
    uid: usize,
    is_split_mode: bool,
    common_header: Option<&str>,
    common_deps: &FxHashSet<String>,
    system_typedefs: &[(String, String)],
    primary_functions: Option<&FxHashSet<String>>,  // For chunked mode: functions to keep full body
    extern_functions: &FxHashMap<String, String>,
    extern_variables: &FxHashMap<String, String>,  // Bug48: extern const struct declarations
    static_funcptr_vars: &FxHashMap<String, String>,  // Bug71: static function pointer variables
    project_types: &ProjectTypes,  // Pre-computed project types (OPTIMIZATION)
    shared_maps: &SharedMaps,  // Pre-computed shared maps for typedef name lookup (Bug47)
    code_identifiers: &CodeIdentifiers,  // Pre-tokenized identifiers (Optimization #1)
    position_index: &PositionIndex,  // Pre-computed position index (Optimization #2)
    target_function: Option<&str>,  // Bug68 fix: target function key for this PU (for is_primary check)
) {
    if DEBUG_TAGS_ENABLED.load(Ordering::Relaxed) {
        eprintln!("DEBUG print_necessary_units: uid={} necessary.len()={} is_split={}", uid, necessary.len(), is_split_mode);
        for u in necessary.iter().take(3) {
            let body_len = pu.get(u).map(|s| s.len()).unwrap_or(0);
            eprintln!("  necessary: {} body_len={}", u, body_len);
        }
    }
    if std::env::var("DEBUG_EXTERN").is_ok() {
        eprintln!("DEBUG print_necessary_units: uid={}, extern_variables.len()={}", uid, extern_variables.len());
    }
    let mut c: usize = 0;
    let mut k: usize = 0;
    let mut pass1_ran = false; // Track whether Pass 1 (typedefs) was executed
    // Deduplicate Pass 1 bodies: typedef struct Foo {...} Bar creates both
    // "typedef:Bar:file" and "struct:Foo:file" with the same body. Track seen
    // bodies by their trimmed content to avoid emitting duplicates.
    let mut pass1_seen_bodies: FxHashSet<String> = FxHashSet::default();
    // Build a set of system typedef names for quick lookup
    let _system_typedef_names: FxHashSet<&str> = system_typedefs.iter()
        .map(|(name, _)| name.as_str())
        .collect();

    // OPTIMIZATION: Use pre-computed project_types instead of rebuilding per-PU
    // This eliminates O(pu_order) iteration for each of 2485 PUs (12M iterations saved)

    // OPTIMIZATION: Use BufferedOutput to collect all content in memory, write once at the end
    // This reduces ~10+ syscalls per PU to just 1

    // First pass: count necessary units and determine output file path
    // OPTIMIZATION #6: Iterate over necessary (small) instead of pu_order (large)
    // This reduces O(pu_order.len()) to O(necessary.len()) per PU
    let mut output_file_path: Option<String> = None;
    let mut first_pos: Option<usize> = None;
    let mut total_non_enumerator_count: usize = 0;  // Total count including types (for early exit check)
    for u in necessary.iter() {
        // OPTIMIZATION: Use PuType::from_key for fast byte-level enumerator check
        if PuType::from_key(u) != PuType::Enumerator {
            total_non_enumerator_count += 1;
            // Track the unit with smallest position for file name
            if let Some(pos) = position_index.get_pos(u) {
                if first_pos.is_none() || pos < first_pos.unwrap() {
                    first_pos = Some(pos);
                    let a: Vec<&str> = u.split(":").collect();
                    if a.len() >= 3 {
                        let file_str = a[2];
                        output_file_path = Some(if uid == 0 {
                            file_str.to_owned() + ".pu.c"
                        } else {
                            format!("{}_{}.pu.c", file_str, uid)
                        });
                    }
                }
            }
            // Bug69 fix: Count ALL non-enumerator items in c, not just functions/variables
            // This ensures k < c is almost always true, so dependent functions become declarations
            // The Bug60 fix that only counted functions/variables caused more function bodies
            // to be output, which have unresolved references and cause compilation errors
            c += 1;
        }
    }

    // Early return if nothing to write (use total count, not just functions/variables)
    if total_non_enumerator_count == 0 || output_file_path.is_none() {
        return;
    }

    let output_file = output_file_path.unwrap();
    // Pre-allocate buffer (estimate ~1KB per necessary unit)
    let mut buffered = BufferedOutput::with_capacity(total_non_enumerator_count * 1024);

    // OPTIMIZATION: Pre-sort necessary items by position once
    // This allows iterating over necessary (small) instead of pu_order (large) in later passes
    // Reduces O(pu_order.len() * num_passes) to O(necessary.len() * num_passes)
    // OPTIMIZATION: Use PuType::from_key for fast byte-level enumerator check
    let mut necessary_sorted: Vec<&String> = necessary.iter()
        .filter(|u| PuType::from_key(u) != PuType::Enumerator)
        .collect();
    necessary_sorted.sort_by_key(|u| position_index.get_pos(*u).unwrap_or(usize::MAX));

    // Add system typedefs at the beginning of each split unit (if in split mode)
    // Filter out any typedefs that conflict with project definitions (project takes precedence)
    if is_split_mode && !system_typedefs.is_empty() && c > 0 {
        // Debug: check if __uint16_t is in system_typedefs
        let debug = std::env::var("DEBUG_TYPEDEFS").is_ok();
        if debug {
            eprintln!("DEBUG: system_typedefs count: {}", system_typedefs.len());
            for (name, _) in system_typedefs.iter().take(50) {
                if name.contains("uint16") {
                    eprintln!("DEBUG: Found in system_typedefs: {}", name);
                }
            }
            if project_types.contains("__uint16_t") {
                eprintln!("DEBUG: __uint16_t is in project_types - will be filtered!");
            } else {
                eprintln!("DEBUG: __uint16_t is NOT in project_types");
            }
        }
        // Filter system typedefs to exclude those defined in project
        let filtered_typedefs: String = system_typedefs.iter()
            .filter(|(name, _)| !project_types.contains(name.as_str()))
            .map(|(_, line)| line.as_str())
            .collect::<Vec<&str>>()
            .join("\n");

        if !filtered_typedefs.is_empty() {
            buffered.append_raw(&filtered_typedefs);
            buffered.append_raw("\n");
        }
    }

    // Bug72: Compute extern function/variable declarations but DON'T output yet
    // These need to be output AFTER Pass 1 (typedefs) because they may use project typedefs
    // like `langType` that are defined in the project, not system headers
    let mut extern_func_decls_output = String::new();
    let mut extern_var_decls_output = String::new();

    // Bug17 fix: Compute all_code_identifiers if either extern_functions or extern_variables is non-empty
    // Previously extern_variables processing was nested inside extern_functions check, causing
    // extern variable declarations to be skipped when there were no extern functions
    let needs_extern_processing = is_split_mode && (!extern_functions.is_empty() || !extern_variables.is_empty());

    if needs_extern_processing {
        // OPTIMIZATION #1: Use pre-tokenized identifiers instead of tokenizing all_code
        // This eliminates O(code_size) tokenization per PU
        let all_code_identifiers: FxHashSet<&str> = code_identifiers.get_union(necessary.iter());

        // Process extern functions if any exist
        if !extern_functions.is_empty() {
            // OPTIMIZATION #3: Use pre-computed extern-declared functions instead of regex scan
            // This eliminates O(code_size) regex scan per PU
            // Bug65 fix: Skip prototype PUs when computing already_declared because:
            // - Prototype PUs contain extern declarations from preprocessed files (e.g., "extern int close(...)")
            // - But these prototypes are NOT output to the final PU file
            // - So we shouldn't skip adding our simplified extern declarations based on them
            let already_declared: FxHashSet<&str> = code_identifiers.get_extern_funcs_union(
                necessary.iter().filter(|key| PuType::from_key(key) != PuType::Prototype)
            );

            // Build set of functions defined in this PU (not just declared)
            // Bug31 fix: Skip extern declarations for functions that are defined in this PU
            // because the extern decl appears before the struct definition in our output order,
            // causing "conflicting types" errors due to incomplete struct types in parameters
            let defined_in_pu: FxHashSet<&str> = necessary.iter()
                .filter_map(|key| {
                    if PuType::from_key(key) == PuType::Function {
                        extract_key_name(key)
                    } else {
                        None
                    }
                })
                .collect();

            // Find which extern functions are actually referenced but NOT already declared
            if std::env::var("DEBUG_BUG66").is_ok() {
                eprintln!("DEBUG Bug66: uid={}", uid);
                eprintln!("DEBUG Bug66: extern_functions.keys() = {:?}", extern_functions.keys().collect::<Vec<_>>());
                let ctype_in_ids = all_code_identifiers.contains("__ctype_b_loc");
                let ctype_in_declared = already_declared.contains("__ctype_b_loc");
                eprintln!("DEBUG Bug66: __ctype_b_loc: in_ids={}, in_declared={}", ctype_in_ids, ctype_in_declared);
            }
            let mut referenced_externs: Vec<&str> = extern_functions.keys()
                .filter(|func_name| {
                    let name_str = func_name.as_str();
                    if !all_code_identifiers.contains(name_str) {
                        return false;
                    }
                    // Bug66 fix: Don't skip glibc ctype internal functions even if already_declared.
                    // These functions (__ctype_b_loc, __ctype_tolower_loc, __ctype_toupper_loc)
                    // may appear in code spans from glibc headers, but those declarations
                    // may span multiple lines and not be output properly. Always output the
                    // canonical declaration from stdlib_prototypes.
                    let is_glibc_ctype_internal = name_str.starts_with("__ctype_");
                    if !is_glibc_ctype_internal && already_declared.contains(name_str) {
                        return false;
                    }
                    // Bug31 fix: Skip if function is defined in this PU
                    if defined_in_pu.contains(name_str) {
                        return false;
                    }
                    true
                })
                .map(|s| s.as_str())
                .collect();
            referenced_externs.sort();

            if !referenced_externs.is_empty() {
                let extern_decls: String = referenced_externs.iter()
                    .filter_map(|name| extern_functions.get(*name))
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("\n");

                extern_func_decls_output.push_str("// Extern function declarations (not captured by ctags)\n");
                extern_func_decls_output.push_str(&extern_decls);
                extern_func_decls_output.push('\n');
            }
        }

        // Bug17 fix: Process extern variables INDEPENDENTLY of extern functions
        // Previously this was nested inside the extern_functions check
        if !extern_variables.is_empty() {
            if std::env::var("DEBUG_EXTERN").is_ok() {
                eprintln!("DEBUG PU {}: extern_variables has {} entries", uid, extern_variables.len());
                eprintln!("DEBUG PU {}: all_code_identifiers has {} entries", uid, all_code_identifiers.len());
                for var_name in extern_variables.keys() {
                    let in_code = all_code_identifiers.contains(var_name.as_str());
                    eprintln!("  {} -> in_code={}", var_name, in_code);
                }
            }

            // Bug78 fix: Build a set of available type names from the necessary set
            // This is used to filter out extern variable declarations that use types not in the PU
            let available_types: FxHashSet<&str> = necessary.iter()
                .filter_map(|u| {
                    let pu_type = PuType::from_key(u);
                    if pu_type == PuType::Typedef || pu_type == PuType::Enum {
                        // Extract name from "typedef:name:file" or "enum:name:file"
                        let parts: Vec<&str> = u.splitn(3, ':').collect();
                        if parts.len() >= 2 {
                            Some(parts[1])
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                })
                .collect();

            // Find which extern variables are actually referenced but NOT already declared
            let mut referenced_extern_vars: Vec<&str> = extern_variables.keys()
                .filter(|var_name| {
                    // Check if variable name is referenced in the code
                    all_code_identifiers.contains(var_name.as_str())
                })
                .map(|s| s.as_str())
                .collect();
            referenced_extern_vars.sort();

            if !referenced_extern_vars.is_empty() {
                // Bug78 fix: Filter out extern variable declarations that use types not in necessary set
                let extern_var_decls: String = referenced_extern_vars.iter()
                    .filter_map(|name| extern_variables.get(*name))
                    .filter(|decl| {
                        // Check if the declaration uses a custom type that's not available
                        if let Some(type_name) = extract_type_from_extern_var_decl(decl) {
                            // Skip if the type is not in the available types
                            // (typedefs and enums that are in the necessary set)
                            let is_available = available_types.contains(type_name)
                                || shared_maps.all_typedef_names.contains(type_name)
                                    && necessary.iter().any(|u| u.starts_with(&format!("typedef:{}:", type_name)));
                            if !is_available && std::env::var("DEBUG_BUG78").is_ok() {
                                eprintln!("DEBUG Bug78: Skipping extern var decl with unavailable type {}: {}", type_name, decl);
                            }
                            is_available
                        } else {
                            // Basic type - always include
                            true
                        }
                    })
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("\n");

                if !extern_var_decls.is_empty() {
                    extern_var_decls_output.push_str("// Extern variable declarations (not captured by ctags)\n");
                    extern_var_decls_output.push_str(&extern_var_decls);
                    extern_var_decls_output.push('\n');
                }
            }
        }
    }

    // Add common header include if present
    if let Some(header_file) = common_header {
        buffered.append_raw(&format!("#include \"{}\"\n", header_file));
    }

    // Track functions that receive K&R forward declarations - used to avoid writing
    // conflicting full-signature declarations (prototypes) later
    let mut funcs_with_forward_decl: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Bug32 fix: Track functions that were output early as declarations (returning non-int types)
    // These need to be skipped in the main output loop to avoid duplicates
    let mut early_output_funcs: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Generate minimal forward declarations for functions referenced by variables.
    // This handles cases where variables have function pointer initializers like:
    //   static struct { func_ptr fn; } syscalls[] = { { func }, { func2 } };
    // where func and func2 are defined later in the file.
    // We need forward decls for:
    // 1. Functions NOT in pu_order slice (defined elsewhere)
    // 2. Functions IN pu_order but appearing AFTER a variable that references them
    // We use K&R-style declarations (no parameters) to avoid needing unknown types.
    if std::env::var("DEBUG_BUG33").is_ok() {
        eprintln!("DEBUG Bug33 pre-block: uid={}, is_split_mode={}, c={}", uid, is_split_mode, c);
    }
    if is_split_mode && c > 0 {
        // OPTIMIZATION #2 & #6: Use pre-computed position_index and iterate over necessary
        // This eliminates O(pu_order.len()) iteration for each of 2485 PUs

        // Collect code from variables and functions to find function pointer references
        // Variables: look for function pointers in initializers
        // Functions: look for function references used as values (not called)
        let code_with_positions: Vec<(usize, &String)> = necessary.iter()
            .filter(|u| PuType::key_is_func_or_var(u))
            .filter_map(|u| position_index.get_pos(u).map(|pos| (pos, u)))
            .collect();

        // OPTIMIZATION #1: Use pre-tokenized identifier sets instead of re-tokenizing
        // Get pre-computed identifiers for each necessary code block
        let code_identifier_sets: Vec<(usize, &String, Option<&FxHashSet<String>>)> = code_with_positions
            .iter()
            .map(|(pos, key)| (*pos, *key, code_identifiers.get(*key)))
            .collect();

        // OPTIMIZATION: Build inverted index from identifier name -> Vec<(pos, key)> that reference it.
        // This transforms the O(tags × positions) forward-decl check into O(positions × identifiers + tags).
        let mut name_to_referencing_positions: FxHashMap<&str, Vec<(usize, &String)>> =
            FxHashMap::with_capacity_and_hasher(code_identifier_sets.len() * 8, Default::default());
        for i in 0..code_identifier_sets.len() {
            let (pos, key, ref ids_opt) = code_identifier_sets[i];
            if let Some(ref ids) = *ids_opt {
                for id in ids.iter() {
                    name_to_referencing_positions
                        .entry(id.as_str())
                        .or_default()
                        .push((pos, key));
                }
            }
        }

        // Find all function names referenced in variables/functions that need forward declarations
        let mut forward_decl_funcs: Vec<(String, String)> = Vec::new();
        let mut seen_funcs: std::collections::HashSet<&String> = std::collections::HashSet::new();

        for (func_name, units) in tags.iter() {
            // Check if this is a function and get its unit key
            for unit in units.iter() {
                if PuType::from_key(unit) == PuType::Function {
                    // Build full unit key - use efficient parser
                    if let Some((_, file_part)) = parse_key_type_rest(unit) {
                        let full_key = format!("function:{}:{}", func_name, file_part);

                        // Check if function is referenced by any variable or other function.
                        // Use inverted index to skip functions not referenced at all (O(1) lookup).
                        let mut needs_forward_decl = false;
                        if let Some(refs) = name_to_referencing_positions.get(func_name.as_str()) {
                            for (ref_pos, ref_key) in refs {
                                // Skip self-reference
                                if *ref_key == &full_key {
                                    continue;
                                }
                                // Check if function is NOT in pu_order or appears AFTER this referrer
                                if let Some(func_pos) = position_index.get_pos(&full_key) {
                                    if func_pos > *ref_pos {
                                        needs_forward_decl = true;
                                        break;
                                    }
                                } else {
                                    // Function is NOT in pu_order slice - always need forward decl
                                    needs_forward_decl = true;
                                    break;
                                }
                            }
                        }

                        if needs_forward_decl && !seen_funcs.contains(func_name) {
                            // Get the function code to generate forward declaration
                            // If not available, use empty string (will generate basic int decl)
                            let func_code = pu.get(&full_key)
                                .map(|s| s.as_str())
                                .unwrap_or("");
                            forward_decl_funcs.push((func_name.clone(), func_code.to_string()));
                            seen_funcs.insert(func_name);
                            funcs_with_forward_decl.insert(func_name.clone());
                        }
                    }
                }
            }
        }

        // Bug70 fix: Also check for functions that ONLY have prototypes (no function definition).
        // These are typically external functions from .pro header files (e.g., ex_append from ex_cmds.pro).
        // When used as function pointers in array initializers (like cmdnames[]), they need K&R declarations.
        // Skip functions already handled above (those with function definitions).
        // Track these separately so Bug24 logic doesn't try to output their full prototypes (which may
        // reference types not defined in this PU).
        let mut prototype_only_funcs: std::collections::HashSet<String> = std::collections::HashSet::new();
        for (func_name, units) in tags.iter() {
            // Skip if already handled (has function definition)
            if seen_funcs.contains(func_name) {
                continue;
            }
            // Check if this has a prototype but NO function definition
            let has_prototype = units.iter().any(|u| PuType::from_key(u) == PuType::Prototype);
            let has_function = units.iter().any(|u| PuType::from_key(u) == PuType::Function);
            if has_prototype && !has_function {
                // Check if this prototype-only function is referenced by code in necessary
                let mut is_referenced_in_necessary = false;
                for (_, _, ref_identifiers_opt) in &code_identifier_sets {
                    if let Some(ref_identifiers) = ref_identifiers_opt {
                        if ref_identifiers.contains(func_name) {
                            is_referenced_in_necessary = true;
                            break;
                        }
                    }
                }
                if is_referenced_in_necessary {
                    // Get the prototype code to extract return type for proper K&R declaration
                    // Instead of empty string (which defaults to `int`), pass the prototype
                    // so generate_minimal_forward_decl can extract the actual return type.
                    //
                    // Use prototype_map instead of tags - prototype_map has full keys like
                    // "prototype:funcname:filename" that can be used to look up code in pu map.
                    let proto_code = shared_maps.prototype_map.get(func_name)
                        .and_then(|keys| keys.first())
                        .and_then(|k| pu.get(k))
                        .map(|s| s.to_string())
                        .unwrap_or_default();
                    forward_decl_funcs.push((func_name.clone(), proto_code));
                    seen_funcs.insert(func_name);
                    funcs_with_forward_decl.insert(func_name.clone());
                    prototype_only_funcs.insert(func_name.clone());
                }
            }
        }

        // Bug50 fix: Also find identifiers in variable initializers that look like function
        // references but are NOT in tags at all (ctags didn't capture them).
        // These are typically function prototypes that ctags missed due to custom types
        // (like `void f_ch_canread(typval_T *argvars, typval_T *rettv);`).
        // We generate K&R style forward declarations for these to avoid "undeclared" errors.
        //
        // OPTIMIZATION: Use pre-computed sets from SharedMaps instead of rebuilding per-PU
        // all_tag_names: all names that have at least one tag entry
        // non_function_names: names that have at least one non-function unit (typedef, struct, etc.)

        // Collect unknown identifiers from variable initializers
        // These might be function pointers not captured by ctags
        let mut unknown_func_refs: FxHashSet<String> = FxHashSet::default();

        for (_, ref_key, ref_identifiers_opt) in &code_identifier_sets {
            // Only scan variable initializers, not function bodies
            // (function bodies are more likely to have local variables with arbitrary names)
            if !PuType::key_is_variable(ref_key) {
                continue;
            }

            // Skip if no pre-computed identifiers
            let ref_identifiers = match ref_identifiers_opt {
                Some(ids) => ids,
                None => continue,
            };

            for ident in ref_identifiers.iter() {
                let ident_str = ident.as_str();
                // Skip C keywords
                const C_KEYWORDS: &[&str] = &[
                    "if", "else", "while", "for", "do", "switch", "case", "break",
                    "continue", "return", "goto", "sizeof", "typeof", "struct",
                    "union", "enum", "typedef", "static", "extern", "const",
                    "volatile", "void", "int", "char", "short", "long", "float",
                    "double", "signed", "unsigned", "auto", "register", "inline",
                    "restrict", "NULL", "true", "false", "default"
                ];
                if C_KEYWORDS.contains(&ident_str) {
                    continue;
                }

                // Skip standard library function names - these already have proper declarations
                // from system headers and adding K&R declarations would conflict
                const STD_LIB_FUNCS: &[&str] = &[
                    // math.h
                    "acos", "asin", "atan", "atan2", "ceil", "cos", "cosh", "exp", "fabs",
                    "floor", "fmod", "frexp", "ldexp", "log", "log10", "modf", "pow",
                    "sin", "sinh", "sqrt", "tan", "tanh", "trunc", "round", "copysign",
                    "hypot", "log2", "exp2", "expm1", "log1p", "cbrt", "erf", "erfc",
                    "lgamma", "tgamma", "nearbyint", "rint", "lrint", "llrint", "lround",
                    "llround", "fdim", "fmax", "fmin", "fma", "nan", "nanf", "nanl",
                    "isnan", "isinf", "isfinite", "isnormal", "signbit", "fpclassify",
                    // stdio.h
                    "printf", "fprintf", "sprintf", "snprintf", "scanf", "fscanf", "sscanf",
                    "fopen", "fclose", "fread", "fwrite", "fgets", "fputs", "fgetc", "fputc",
                    "getc", "putc", "getchar", "putchar", "ungetc", "feof", "ferror",
                    "clearerr", "fseek", "ftell", "rewind", "fgetpos", "fsetpos", "fflush",
                    "perror", "remove", "rename", "tmpfile", "tmpnam", "setvbuf", "setbuf",
                    "vprintf", "vfprintf", "vsprintf", "vsnprintf", "vscanf", "vfscanf", "vsscanf",
                    // stdlib.h
                    "malloc", "calloc", "realloc", "free", "abort", "exit", "atexit",
                    "system", "getenv", "bsearch", "qsort", "abs", "labs", "llabs",
                    "div", "ldiv", "lldiv", "rand", "srand", "atoi", "atol", "atoll",
                    "atof", "strtol", "strtoll", "strtoul", "strtoull", "strtof", "strtod", "strtold",
                    // string.h
                    "memcpy", "memmove", "memset", "memcmp", "memchr", "strcpy", "strncpy",
                    "strcat", "strncat", "strcmp", "strncmp", "strcoll", "strxfrm",
                    "strchr", "strrchr", "strspn", "strcspn", "strpbrk", "strstr", "strtok",
                    "strlen", "strerror", "strdup", "strndup",
                    // ctype.h
                    "isalnum", "isalpha", "iscntrl", "isdigit", "isgraph", "islower",
                    "isprint", "ispunct", "isspace", "isupper", "isxdigit", "tolower", "toupper",
                    // time.h
                    "time", "difftime", "mktime", "strftime", "gmtime", "localtime",
                    "asctime", "ctime", "clock", "timespec_get",
                    // unistd.h
                    "read", "write", "close", "lseek", "fork", "execve", "execv", "execvp",
                    "pipe", "dup", "dup2", "getcwd", "chdir", "rmdir", "unlink", "access",
                    "sleep", "usleep", "alarm", "pause", "getpid", "getppid", "getuid", "geteuid",
                    "getgid", "getegid", "setuid", "setgid", "isatty", "ttyname", "link", "symlink",
                    "readlink", "truncate", "ftruncate", "sync", "fsync", "fdatasync",
                    // fcntl.h
                    "open", "creat", "fcntl",
                    // sys/stat.h
                    "stat", "fstat", "lstat", "chmod", "fchmod", "mkdir", "umask",
                    // signal.h
                    "signal", "raise", "kill", "sigaction", "sigemptyset", "sigfillset",
                    "sigaddset", "sigdelset", "sigismember", "sigprocmask", "sigpending",
                    "sigsuspend", "sigwait",
                    // dirent.h
                    "opendir", "closedir", "readdir", "rewinddir", "seekdir", "telldir",
                    // crypt.h
                    "crypt",
                    // locale.h
                    "setlocale", "localeconv",
                    // iconv.h
                    "iconv", "iconv_open", "iconv_close",
                    // dlfcn.h
                    "dlopen", "dlclose", "dlsym", "dlerror",
                    // socket.h
                    "socket", "bind", "listen", "accept", "connect", "send", "recv",
                    "sendto", "recvfrom", "shutdown", "getsockname", "getpeername",
                    "getsockopt", "setsockopt", "select", "poll",
                    // netdb.h
                    "gethostbyname", "gethostbyaddr", "getaddrinfo", "freeaddrinfo", "getnameinfo",
                    // misc
                    "index", "rindex", "bcopy", "bzero", "bcmp", "flock", "mmap", "munmap",
                    "mprotect", "msync", "mlock", "munlock", "mlockall", "munlockall",
                    "getline", "getdelim", "popen", "pclose", "fileno", "fdopen",
                    "wait", "waitpid", "gettimeofday", "settimeofday", "fseeko", "ftello",
                    "bindtextdomain", "bind_textdomain_codeset", "textdomain", "gettext",
                    "dgettext", "dcgettext", "ngettext", "dngettext", "dcngettext",
                ];
                if STD_LIB_FUNCS.contains(&ident_str) {
                    continue;
                }

                // Bug53 fix: Skip if it's already in extern_functions
                // These have extern declarations output earlier (like "extern void* fopen64();")
                // and generating a K&R declaration (like "int fopen64();") would conflict
                if extern_functions.contains_key(ident_str) {
                    continue;
                }

                // Bug78 fix: Skip if it's a known extern variable
                // These are variables like "environ" (POSIX) that have extern declarations
                // and generating a K&R function declaration would conflict
                if extern_variables.contains_key(ident_str) {
                    continue;
                }

                // Skip if it's a known tag name (using pre-computed set from SharedMaps)
                if shared_maps.all_tag_names.contains(ident_str) {
                    continue;
                }

                // Skip if it's a non-function name (type, variable) (using pre-computed set from SharedMaps)
                if shared_maps.non_function_names.contains(ident_str) {
                    continue;
                }

                // Skip if it looks like a type name (common suffixes)
                if ident_str.ends_with("_t") || ident_str.ends_with("_T") || ident_str.ends_with("_s") {
                    continue;
                }

                // Skip if it's already in forward_decl_funcs
                if funcs_with_forward_decl.contains(ident_str) {
                    continue;
                }

                // Skip predefined macros and C keywords that gcc/clang might define
                const PREDEFINED_MACROS: &[&str] = &[
                    "linux", "unix", "__linux", "__linux__", "__unix", "__unix__",
                    "__GNUC__", "__clang__", "__STDC__", "__cplusplus", "__OBJC__",
                    "_Bool", "__bool", "bool", "_Complex", "_Imaginary",
                    "__attribute__", "__extension__", "__asm__", "__volatile__",
                    "__restrict", "__restrict__", "__inline", "__inline__",
                    "__alignof__", "__typeof__", "__builtin_va_list",
                    "X", "Y", "Z", "R", "G", "B", "A", "W", "H", "S", "L", "V",  // Single letter names often macros
                ];
                if PREDEFINED_MACROS.contains(&ident_str) {
                    continue;
                }

                // Skip very short names (1-2 chars) - these are often macros or variables
                if ident_str.len() <= 2 {
                    continue;
                }

                // Skip names that start with underscore followed by uppercase (reserved identifiers)
                if ident_str.starts_with('_') && ident_str.chars().nth(1).map_or(false, |c| c.is_uppercase()) {
                    continue;
                }

                // This identifier is used in a variable initializer but not in tags
                // It's likely a function reference - add it to forward declarations
                unknown_func_refs.insert(ident.to_string());
            }
        }

        // Bug50 fix (part 2): For unknown function references, check if the function
        // definition exists in the PU code (even if ctags didn't capture it).
        // If the definition exists, DON'T generate a forward declaration - the definition
        // will be output and would conflict with our K&R declaration.
        // Only generate K&R declarations for functions that are truly external.
        //
        // Search strategy: look for "funcname(" pattern in all PU code entries that
        // appear to be function definitions (not just calls).
        for func_name in unknown_func_refs {
            // Check if this function has a definition/prototype in the PU.
            // The function might be:
            // 1. In a function: or prototype: entry (direct match)
            // 2. Embedded in another entry's code span (ctags captures adjacent code)
            //
            // We need to search ALL necessary entries' code content, not just function entries,
            // because prototypes are often captured as part of adjacent declarations.
            let mut func_defined_in_pu = false;

            // Build a pattern that matches function definition/prototype signatures
            // Pattern: return_type func_name(
            let pattern = format!("{}(", func_name);

            // Search ALL necessary entries for a function definition/prototype
            for u in necessary.iter() {
                if let Some(code) = pu.get(u) {
                    if !code.contains(&pattern) {
                        continue;
                    }

                    // Verify this is a function definition/prototype, not just a call
                    // Check if the pattern appears at the start of a line (with return type)
                    // or on a line that looks like a signature
                    let mut prev_line_is_type = false;
                    for line in code.lines() {
                        let trimmed = line.trim();

                        // Skip preprocessor lines
                        if trimmed.starts_with('#') {
                            continue;
                        }

                        // Check if this line is a return type line (void, static void, etc.)
                        // Use optimized function instead of multiple contains() calls
                        let is_type_line = is_type_only_line(trimmed);

                        // Check if this line contains the function name at the start
                        // (K&R style where return type is on previous line)
                        if trimmed.starts_with(&func_name) &&
                           trimmed[func_name.len()..].trim_start().starts_with('(') {
                            // Function name at start of line - likely K&R style
                            // Check if previous line was a return type
                            if prev_line_is_type {
                                func_defined_in_pu = true;
                                break;
                            }
                        }

                        // Check if this line has return type + function name on same line
                        if let Some(pos) = trimmed.find(&pattern) {
                            let before = &trimmed[..pos];
                            // Check for return type indicators before function name
                            // Use optimized function instead of multiple contains() calls
                            if !before.is_empty() && contains_return_type_keyword(before) {
                                func_defined_in_pu = true;
                                break;
                            }
                        }

                        prev_line_is_type = is_type_line;
                    }

                    if func_defined_in_pu {
                        break;
                    }
                }
            }

            // Also check if there's a direct function:/prototype: entry in the pu map
            if !func_defined_in_pu {
                for (key, _) in pu.iter() {
                    if !PuType::key_is_func_or_proto(key) {
                        continue;
                    }
                    // Extract function name from key: "function:name:file" or "prototype:name:file"
                    // Use efficient parser instead of split().collect()
                    if let Some(name) = extract_key_name(key) {
                        if name == func_name {
                            func_defined_in_pu = true;
                            break;
                        }
                    }
                }
            }

            if func_defined_in_pu {
                // Function definition/prototype exists in the PU.
                // DO NOT generate a K&R forward declaration - it would conflict with the
                // actual definition that will be output later.
                // The function's own prototype/definition provides its declaration.
                //
                // NOTE: This might cause "undeclared" warnings if the variable using the
                // function appears before the function definition, but that's preferable
                // to "conflicting types" errors from wrong return type in K&R declaration.
                continue;
            } else {
                // Function is truly external - generate basic K&R declaration
                forward_decl_funcs.push((func_name.clone(), String::new()));
                funcs_with_forward_decl.insert(func_name);
            }
        }

        // Bug33 fix: Output source file prototypes UNCONDITIONALLY (before forward decl check).
        // Previously this code was inside "if !forward_decl_funcs.is_empty()" which caused
        // prototype output to be skipped when no forward declarations were needed.
        // But source file prototypes (captured by ctags) should always be output regardless.
        let debug_bug33 = std::env::var("DEBUG_BUG33").is_ok();

        // First, identify functions that have full prototypes (not K&R style)
        let funcs_with_full_prototypes: std::collections::HashSet<String> = pu_order.iter()
            .filter(|u| necessary.contains(*u))
            .filter(|u| PuType::from_key(u) == PuType::Prototype)
            .filter_map(|u| {
                if let Some(code) = pu.get(u) {
                    let trimmed = code.trim();
                    // Use rfind to get the LAST parentheses pair (function params)
                    // This avoids matching __attribute__((...)) which appears before params
                    if let Some(close_paren) = trimmed.rfind(");") {
                        if let Some(open_paren) = trimmed[..close_paren].rfind('(') {
                            let params = trimmed[open_paren + 1..close_paren].trim();
                            if !params.is_empty() && params != "void" {
                                if let Some(name) = extract_key_name(u) {
                                    return Some(name.to_string());
                                }
                            }
                        }
                    }
                }
                None
            })
            .collect();

        if debug_bug33 && uid == 1 {
            eprintln!("DEBUG Bug33: uid={}, funcs_with_full_prototypes={:?}", uid, funcs_with_full_prototypes);
            // Debug: show all entries in pu that contain "get_schema" in code
            for (key, code) in pu.iter() {
                if code.contains("get_schema") {
                    let first_100: String = code.chars().take(100).collect();
                    eprintln!("DEBUG Bug33 PU: key={}, code={:?}", key, first_100);
                }
            }
        }

        // Output prototypes, skipping K&R ones if full prototype exists for same function
        let mut proto_decls = String::new();
        for u in pu_order.iter() {
            if !necessary.contains(u) || common_deps.contains(u) {
                continue;
            }
            let u_type = PuType::from_key(u);
            if u_type == PuType::Prototype {
                if let Some(code) = pu.get(u) {
                    let trimmed_code = code.trim();
                    if !trimmed_code.is_empty() {
                        // Check if this is a K&R prototype
                        // Use rfind to get the LAST parentheses pair (function params)
                        // This avoids matching __attribute__((...)) which appears before params
                        let is_knr = if let Some(close_paren) = trimmed_code.rfind(");") {
                            if let Some(open_paren) = trimmed_code[..close_paren].rfind('(') {
                                let params = trimmed_code[open_paren + 1..close_paren].trim();
                                params.is_empty() || params == "void"
                            } else {
                                false
                            }
                        } else {
                            false
                        };

                        // Skip K&R prototypes if a full prototype exists
                        if is_knr {
                            if let Some(func_name) = extract_key_name(u) {
                                if funcs_with_full_prototypes.contains(func_name) {
                                    if debug_bug33 {
                                        eprintln!("DEBUG Bug33: SKIPPING K&R prototype for {}", func_name);
                                    }
                                    continue;
                                }
                            }
                        }

                        proto_decls.push_str(code);
                        proto_decls.push('\n');
                    }
                }
            } else if u_type == PuType::Function {
                // Also output early declarations for functions returning non-int types
                if let Some(code) = pu.get(u) {
                    let decl = convert_function_to_declaration(code);
                    let trimmed_decl = decl.trim();
                    let rest = u.strip_prefix("function:").unwrap_or("");
                    let func_name = rest.split(':').next().unwrap_or("");
                    let returns_pointer = if !func_name.is_empty() {
                        if let Some(name_pos) = trimmed_decl.find(&format!("{}(", func_name)) {
                            let return_type = &trimmed_decl[..name_pos];
                            return_type.contains('*')
                        } else {
                            trimmed_decl.starts_with("void *") ||
                            trimmed_decl.starts_with("void*") ||
                            trimmed_decl.starts_with("char *") ||
                            trimmed_decl.starts_with("char*") ||
                            trimmed_decl.starts_with("static void *") ||
                            trimmed_decl.starts_with("static void*") ||
                            trimmed_decl.starts_with("static char *") ||
                            trimmed_decl.starts_with("static char*")
                        }
                    } else {
                        false
                    };
                    if !trimmed_decl.is_empty() && returns_pointer {
                        proto_decls.push_str(&decl);
                        proto_decls.push('\n');
                        early_output_funcs.insert(u.clone());
                    }
                }
            }
        }

        // NOTE: proto_decls is accumulated here but output later after Pass 1 (typedefs)
        // This ensures type names like Schema, sqlite3 are defined before declarations use them

        // Generate and output minimal forward declarations
        if !forward_decl_funcs.is_empty() {
            // Sort by function name for consistent output
            forward_decl_funcs.sort_by(|(a, _), (b, _)| a.cmp(b));

            // Build a set of available type names from the necessary set
            // This includes typedef and struct names that are in the current PU
            // Use efficient parsing instead of split().collect()
            let available_types: std::collections::HashSet<String> = necessary.iter()
                .filter_map(|u| {
                    if let Some((kind, name, _)) = parse_key_parts(u) {
                        // Include typedef and struct types
                        if kind == "typedef" || kind == "struct" {
                            return Some(name.to_string());
                        }
                    }
                    None
                })
                .collect();

            // Find first non-enumerator unit in necessary set and extract file
            let file_str = pu_order.iter()
                .find(|u| !u.contains("enumerator:") && necessary.contains(*u))
                .and_then(|u| parse_key_parts(u))
                .map(|(_, _, f)| f);

            if let Some(_file_str) = file_str {
                let mut forward_decls = String::from("// Forward declarations for functions defined elsewhere\n");

                // First, emit forward struct declarations for structs that have typedefs in the necessary set
                // This allows typedefs like "typedef struct X X;" to be forward-declared
                // We collect all struct names that are in the necessary set
                // Use efficient parsing instead of split().collect()
                let mut struct_names: Vec<String> = necessary.iter()
                    .filter_map(|u| {
                        if let Some((kind, name, _)) = parse_key_parts(u) {
                            if kind == "struct" {
                                return Some(name.to_string());
                            }
                        }
                        None
                    })
                    .collect();
                struct_names.sort();
                struct_names.dedup();

                // Emit forward struct declarations
                for struct_name in &struct_names {
                    forward_decls.push_str(&format!("struct {};\n", struct_name));
                }

                // Emit typedefs that reference the forward-declared structs
                // This allows "VdbeOp *" to be used in function declarations
                // Use efficient parsing instead of split().collect()
                let mut typedef_decls: Vec<String> = necessary.iter()
                    .filter_map(|u| {
                        if let Some((kind, typedef_name, _)) = parse_key_parts(u) {
                            if kind == "typedef" {
                                // Check if there's a struct with the same name
                                if struct_names.contains(&typedef_name.to_string()) {
                                    return Some(format!("typedef struct {} {};", typedef_name, typedef_name));
                                }
                            }
                        }
                        None
                    })
                    .collect();
                typedef_decls.sort();
                typedef_decls.dedup();

                for typedef_decl in &typedef_decls {
                    forward_decls.push_str(typedef_decl);
                    forward_decls.push('\n');
                }

                // Bug33 fix: Build a set of function names that have prototypes OR function definitions
                // (that return pointers) in the necessary set.
                // We should NOT generate K&R forward declarations for these functions because:
                // 1. The prototype/declaration will be output later with the correct return type
                // 2. K&R declarations use void* for custom types (Bug26 fix)
                // 3. void* conflicts with the specific struct* return type in the prototype
                // Example: K&R "static void *sqlite3SchemaGet();" conflicts with
                // prototype "static Schema *sqlite3SchemaGet(sqlite3 *db, Btree *pBt);"
                //
                // Bug62 fix: Only skip K&R declarations if the prototype is in BOTH necessary AND pu_order.
                // If a prototype is in necessary (as a dependency) but not in pu_order, it won't be output,
                // so we still need the K&R declaration.
                // OPTIMIZATION: Use position_index instead of building HashSet from pu_order.iter()
                // pu_order is slice [0..=max_pos], so max_pos = pu_order.len() - 1
                let max_slice_pos = pu_order.len().saturating_sub(1);
                let funcs_with_prototypes: std::collections::HashSet<String> = necessary.iter()
                    .filter_map(|u| {
                        if let Some(rest) = u.strip_prefix("prototype:") {
                            // Format is "prototype:func_name:file_part"
                            // Bug62: Only include if prototype is in pu_order slice (will actually be output)
                            // Uses position_index for O(1) lookup instead of O(pu_order) HashSet build
                            if !position_index.is_valid(u, max_slice_pos) {
                                return None;
                            }
                            if let Some(colon_pos) = rest.find(':') {
                                return Some(rest[..colon_pos].to_string());
                            }
                        }
                        // Bug33 enhancement: Also check function entries that return pointers.
                        // These are converted to declarations (lines 3072-3093) and would conflict
                        // with K&R void* declarations.
                        // Bug59 fix: ONLY check return type, not the entire declaration which includes parameters.
                        if let Some(rest) = u.strip_prefix("function:") {
                            // Format is "function:func_name:file_part"
                            if let Some(colon_pos) = rest.find(':') {
                                let func_name = &rest[..colon_pos];
                                // Check if this function returns a pointer type
                                if let Some(code) = pu.get(u) {
                                    let decl = convert_function_to_declaration(code);
                                    let trimmed_decl = decl.trim();
                                    // Bug59: Extract return type by finding function name position
                                    // Only consider return type, not parameters
                                    if let Some(name_pos) = trimmed_decl.find(&format!("{}(", func_name)) {
                                        let return_type = &trimmed_decl[..name_pos];
                                        // Check if return type contains a pointer
                                        if return_type.contains('*') {
                                            return Some(func_name.to_string());
                                        }
                                    } else if !trimmed_decl.is_empty() &&
                                       (trimmed_decl.starts_with("void *") ||
                                        trimmed_decl.starts_with("void*") ||
                                        trimmed_decl.starts_with("char *") ||
                                        trimmed_decl.starts_with("char*") ||
                                        trimmed_decl.starts_with("static void *") ||
                                        trimmed_decl.starts_with("static void*") ||
                                        trimmed_decl.starts_with("static char *") ||
                                        trimmed_decl.starts_with("static char*")) {
                                        return Some(func_name.to_string());
                                    }
                                }
                            }
                        }
                        None
                    })
                    .collect();

                // Bug74 fix: Also scan necessary entries' CODE content for embedded full prototypes.
                // Ctags often captures blocks of prototypes as part of another entry's code span.
                // These embedded prototypes will be output later and conflict with K&R declarations.
                let mut embedded_prototypes: std::collections::HashSet<String> = std::collections::HashSet::new();
                for u in necessary.iter() {
                    // Skip prototype entries (already handled above)
                    if PuType::from_key(u) == PuType::Prototype {
                        continue;
                    }
                    // Only check entries that will actually be output (in pu_order slice)
                    if !position_index.is_valid(u, max_slice_pos) {
                        continue;
                    }
                    if let Some(code) = pu.get(u) {
                        // Scan each line for prototype patterns
                        for line in code.lines() {
                            let trimmed = line.trim();
                            // Look for lines ending with ); that look like prototypes
                            if !trimmed.ends_with(");") {
                                continue;
                            }
                            // Skip typedefs
                            if trimmed.contains("typedef") {
                                continue;
                            }
                            // Bug74 fix: Only skip pure function pointer declarations like:
                            // void (*callback)(int);
                            // These have (* before the first ( of parameter list.
                            // Don't skip prototypes that have function pointer PARAMETERS like:
                            // static int func(void(*)(void*));
                            if let Some(first_paren) = trimmed.find('(') {
                                let before_params = &trimmed[..first_paren];
                                if before_params.contains("(*") {
                                    continue;
                                }
                            }
                            // Bug74 fix: Extract function name from prototype line
                            // Handle __attribute__((...)) before function name by stripping it first
                            let mut search_str = trimmed;

                            // Strip __attribute__((...)) prefix if present
                            // __attribute__ can appear as: __attribute__((xxx)) or __attribute__(xxx)
                            while let Some(attr_pos) = search_str.find("__attribute__") {
                                // Find matching closing paren(s)
                                let after_attr = &search_str[attr_pos + 13..]; // skip "__attribute__"
                                if after_attr.starts_with("((") {
                                    // Double paren: __attribute__((xxx))
                                    if let Some(close_pos) = after_attr.find("))") {
                                        search_str = &search_str[attr_pos + 13 + close_pos + 2..].trim_start();
                                    } else {
                                        break;
                                    }
                                } else if after_attr.starts_with("(") {
                                    // Single paren: __attribute__(xxx)
                                    if let Some(close_pos) = after_attr.find(')') {
                                        search_str = &search_str[attr_pos + 13 + close_pos + 1..].trim_start();
                                    } else {
                                        break;
                                    }
                                } else {
                                    break;
                                }
                            }

                            // Now find first ( in the cleaned string to get function name
                            if let Some(first_paren) = search_str.find('(') {
                                let before_paren = search_str[..first_paren].trim_end();

                                // Bug77 fix: Skip lines that are clearly NOT prototypes:
                                // 1. Contains '=' before the function name (assignment like "x = func(...);")
                                // 2. Contains '->' or '.' before the function name (method call)
                                // These are function CALLS, not prototype declarations.
                                if before_paren.contains('=') || before_paren.contains("->") || before_paren.contains('.') {
                                    continue;
                                }

                                // Extract function name: last word before (
                                let func_name = if let Some(last_space) = before_paren.rfind(|c: char| !c.is_alphanumeric() && c != '_') {
                                    &before_paren[last_space + 1..]
                                } else {
                                    // Bug77 fix: If there's no space/separator before the function name,
                                    // this is likely a function CALL (e.g., "sqlite3_result_error(...);"),
                                    // not a prototype. Prototypes always have a return type before the name.
                                    continue;
                                };

                                // Bug77 fix: Check if word before func_name is a C keyword (return, if, while, etc.)
                                // If so, this is a function call/control statement, not a prototype.
                                // Prototypes have type keywords (int, void, char, etc.) before the function name.
                                let before_func = before_paren[..before_paren.len() - func_name.len()].trim();
                                let last_word_before = before_func.rsplit(|c: char| !c.is_alphanumeric() && c != '_').next().unwrap_or("");
                                if matches!(last_word_before, "return" | "if" | "while" | "for" | "switch" | "case" | "sizeof" | "typeof" | "goto") {
                                    continue;
                                }

                                // Bug77 fix: A valid prototype must end with a type keyword or * (pointer).
                                // If before_func ends with an operator (+, -, *, /, etc.) or comma, it's
                                // part of an expression, not a prototype declaration.
                                // Also skip if before_func is empty (just "funcname(...);" is a call, not prototype).
                                if before_func.is_empty() {
                                    continue;
                                }
                                let last_char = before_func.chars().last().unwrap_or(' ');
                                // If the last char is an operator (except *), it's not a prototype
                                // Note: * is allowed for pointer return types (char*, void*, etc.)
                                if matches!(last_char, '+' | '-' | '/' | '(' | ',' | '<' | '>' | '!' | '&' | '|' | '^' | '%' | '[') {
                                    continue;
                                }

                                if !func_name.is_empty() && func_name != "__attribute__" {
                                    // Check if params are non-empty
                                    if let Some(last_close) = trimmed.rfind(");") {
                                        // Use original trimmed to get full params
                                        if let Some(func_start) = trimmed.find(func_name) {
                                            if let Some(paren_pos) = trimmed[func_start..].find('(') {
                                                let params_start = func_start + paren_pos + 1;
                                                if params_start < last_close {
                                                    let params = trimmed[params_start..last_close].trim();
                                                    if !params.is_empty() {
                                                        embedded_prototypes.insert(func_name.to_string());
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                for (func_name, func_code) in forward_decl_funcs.iter() {
                    // Bug33 fix: Skip K&R declarations for functions that have prototypes
                    if funcs_with_prototypes.contains(func_name) {
                        continue;
                    }
                    // Bug74 fix: Also skip if there's an embedded full prototype in necessary entries
                    if embedded_prototypes.contains(func_name) {
                        continue;
                    }
                    // Bug47 fix: Skip K&R declarations for names that are typedef names
                    // This avoids generating "int char_u();" which conflicts with "typedef unsigned char char_u;"
                    if shared_maps.all_typedef_names.contains(func_name) {
                        continue;
                    }
                    // Bug24 fix: If a prototype exists for this function (even if not in necessary set),
                    // output the actual prototype instead of K&R void* stub. This is critical when the
                    // return value is dereferenced with -> operator (e.g., sqlite3VdbeGetLastOp(v)->opcode).
                    // K&R void* would cause "request for member in something not a structure or union".
                    // Bug31 fix: Skip if the function is in extern_functions - it was already written
                    // by the extern declarations block, and writing it again causes conflicting types
                    // errors for functions with inline struct declarations in parameters.
                    if extern_functions.contains_key(func_name) {
                        continue;
                    }
                    // Bug70 fix: Don't output full prototypes for prototype-only functions (e.g., ex_append).
                    // These prototypes reference types (exarg_T, event_T) that may not be defined in this PU.
                    // Instead, let the K&R declaration below handle them.
                    if !prototype_only_funcs.contains(func_name) {
                        if let Some(proto_units) = shared_maps.prototype_map.get(func_name) {
                            if let Some(full_key) = proto_units.first() {
                                // prototype_map now stores full pu_keys directly (format: "prototype:func_name:filename")
                                if let Some(proto_code) = pu.get(full_key) {
                                    let trimmed = proto_code.trim();
                                    if !trimmed.is_empty() {
                                        forward_decls.push_str(proto_code);
                                        forward_decls.push('\n');
                                        continue;
                                    }
                                }
                            }
                        }
                    }
                    if let Some(decl) = generate_minimal_forward_decl(func_name, func_code, Some(&available_types)) {
                        forward_decls.push_str(&decl);
                        forward_decls.push('\n');
                    }
                }
                forward_decls.push('\n');

                // Bug32 fix: Output ALL prototypes FIRST, before iterating pu_order
                // This ensures prototypes appear before any functions that call them,
                // regardless of source file order. Without this, a prototype that appears
                // later in the source file would still be output after functions that use it.
                //
                // Also output declarations for functions returning non-int types (void*, char*, etc.)
                // These functions need early declarations to avoid GCC's K&R implicit int assumption.
                //
                // Bug61 fix: Iterate over pu_order instead of necessary (HashSet) to preserve
                // source file order. This is critical because some prototypes reference other
                // functions in __attribute__((__malloc__(func, N))) attributes (e.g., fopen
                // references fclose). The referenced function must be declared first.

                // Bug33 fix: First, identify functions that have full prototypes (not K&R style)
                // A K&R prototype has empty parentheses: `int func();`
                // A full prototype has parameter types: `int func(int a, char *b);`
                // When both exist, we should only output the full prototype to avoid conflicts.
                let funcs_with_full_prototypes: std::collections::HashSet<String> = pu_order.iter()
                    .filter(|u| necessary.contains(*u))
                    .filter(|u| PuType::from_key(u) == PuType::Prototype)
                    .filter_map(|u| {
                        if let Some(code) = pu.get(u) {
                            let trimmed = code.trim();
                            // Check if this is a full prototype (has parameters inside parentheses)
                            // K&R style has empty `()` or just `(void)`
                            // Use rfind to get the LAST parentheses pair (function params)
                            // This avoids matching __attribute__((...)) which appears before params
                            if let Some(close_paren) = trimmed.rfind(");") {
                                if let Some(open_paren) = trimmed[..close_paren].rfind('(') {
                                    let params = trimmed[open_paren + 1..close_paren].trim();
                                    // Non-empty and not just "void" means it's a full prototype
                                    if !params.is_empty() && params != "void" {
                                        // Extract function name from key
                                        if let Some(name) = extract_key_name(u) {
                                            return Some(name.to_string());
                                        }
                                    }
                                }
                            }
                        }
                        None
                    })
                    .collect();

                let debug_bug33 = std::env::var("DEBUG_BUG33").is_ok();
                if debug_bug33 {
                    eprintln!("DEBUG Bug33: uid={}, c={}, pu_order.len()={}", uid, c, pu_order.len());
                    eprintln!("DEBUG Bug33: funcs_with_full_prototypes = {:?}", funcs_with_full_prototypes);
                    eprintln!("DEBUG Bug33: About to iterate pu_order for prototypes");
                }
                for u in pu_order.iter() {
                    if !necessary.contains(u) || common_deps.contains(u) {
                        continue;
                    }
                    let u_type = PuType::from_key(u);
                    if u_type == PuType::Prototype {
                        if let Some(code) = pu.get(u) {
                            let trimmed_code = code.trim();
                            if !trimmed_code.is_empty() {
                                // Bug33 fix: Check if this is a K&R prototype and a full prototype exists
                                // Use rfind to get the LAST parentheses pair (function params)
                                // This avoids matching __attribute__((...)) which appears before params
                                let is_knr = if let Some(close_paren) = trimmed_code.rfind(");") {
                                    if let Some(open_paren) = trimmed_code[..close_paren].rfind('(') {
                                        let params = trimmed_code[open_paren + 1..close_paren].trim();
                                        params.is_empty() || params == "void"
                                    } else {
                                        false
                                    }
                                } else {
                                    false
                                };

                                if debug_bug33 {
                                    let func_name = extract_key_name(u).unwrap_or("");
                                    eprintln!("DEBUG Bug33: prototype u={}, func_name={}, is_knr={}, in_full_protos={}, code={}",
                                        u, func_name, is_knr, funcs_with_full_prototypes.contains(func_name), trimmed_code.chars().take(50).collect::<String>());
                                }

                                // Skip K&R prototypes if a full prototype exists for this function
                                if is_knr {
                                    if let Some(func_name) = extract_key_name(u) {
                                        if funcs_with_full_prototypes.contains(func_name) {
                                            if debug_bug33 {
                                                eprintln!("DEBUG Bug33: SKIPPING K&R prototype for {}", func_name);
                                            }
                                            continue;  // Skip this K&R, full prototype will be output
                                        }
                                    }
                                }

                                forward_decls.push_str(code);
                                forward_decls.push('\n');
                            }
                        }
                    } else if u_type == PuType::Function {
                        // Bug32 enhancement: Also output early declarations for functions
                        // returning non-int types (void*, char*, pointers, etc.)
                        if let Some(code) = pu.get(u) {
                            let decl = convert_function_to_declaration(code);
                            let trimmed_decl = decl.trim();
                            // Bug59 fix: Extract return type only, not entire declaration
                            // Avoid matching pointer parameters like "Parse *pParse"
                            let rest = u.strip_prefix("function:").unwrap_or("");
                            let func_name = rest.split(':').next().unwrap_or("");
                            let returns_pointer = if !func_name.is_empty() {
                                if let Some(name_pos) = trimmed_decl.find(&format!("{}(", func_name)) {
                                    let return_type = &trimmed_decl[..name_pos];
                                    return_type.contains('*')
                                } else {
                                    // Fallback: check common pointer return types at start
                                    trimmed_decl.starts_with("void *") ||
                                    trimmed_decl.starts_with("void*") ||
                                    trimmed_decl.starts_with("char *") ||
                                    trimmed_decl.starts_with("char*") ||
                                    trimmed_decl.starts_with("static void *") ||
                                    trimmed_decl.starts_with("static void*") ||
                                    trimmed_decl.starts_with("static char *") ||
                                    trimmed_decl.starts_with("static char*")
                                }
                            } else {
                                false
                            };
                            if !trimmed_decl.is_empty() && returns_pointer {
                                forward_decls.push_str(&decl);
                                forward_decls.push('\n');
                                // Track this function so we skip it in the main output loop
                                early_output_funcs.insert(u.clone());
                            }
                        }
                    }
                }

                // Bug25 fix: Output typedefs and other types FIRST, then K&R forward declarations
                // This ensures typedefs like `typedef unsigned long long mysize_t;` are defined
                // before K&R forward declarations like `static mysize_t func();` that use them.

                // Bug34 fix: Before outputting types, scan type code for embedded function calls
                // and write necessary prototypes IMMEDIATELY (before Pass 1). This handles cases
                // where ctags captures struct code spans that include adjacent function definitions.
                // Those embedded functions may call other functions (like sqlite3_aggregate_context)
                // that need forward declarations before being called.
                let mut type_embedded_calls: std::collections::HashSet<String> = std::collections::HashSet::new();
                // Skip list for C keywords that look like function calls
                const C_KEYWORDS: &[&str] = &["if", "while", "for", "switch", "return", "sizeof", "typeof", "struct", "union", "enum"];

                // OPTIMIZATION #6: Iterate over necessary (small) instead of pu_order (large)
                for u in necessary.iter() {
                    // OPTIMIZATION: Use PuType::from_key for fast byte-level check
                    let pu_type = PuType::from_key(u);
                    if pu_type != PuType::Enumerator {

                        // Only check type definitions
                        if pu_type == PuType::Struct || pu_type == PuType::Union {
                            if let Some(code) = pu.get(u) {
                                // Scan for function calls in this type's code using fast tokenizer
                                for func_name in tokenize_function_calls(code) {
                                    // Skip common C keywords and control structures
                                    if !C_KEYWORDS.contains(&func_name) {
                                        type_embedded_calls.insert(func_name.to_string());
                                    }
                                }
                            }
                        }
                    }
                }

                // Bug74: Pre-scan necessary entries for embedded full prototypes.
                // These will be output later and should NOT have early K&R declarations.
                let mut early_embedded_prototypes: std::collections::HashSet<String> = std::collections::HashSet::new();
                for u in necessary.iter() {
                    if let Some(code) = pu.get(u) {
                        for line in code.lines() {
                            let trimmed = line.trim();
                            if !trimmed.ends_with(");") { continue; }
                            // Skip typedefs
                            if trimmed.contains("typedef") { continue; }
                            // Bug74 fix: Only skip pure function pointer declarations like:
                            // void (*callback)(int);
                            // These have (* before the first ( of parameter list.
                            // Don't skip prototypes that have function pointer PARAMETERS like:
                            // static int func(void(*)(void*));
                            if let Some(first_paren) = trimmed.find('(') {
                                let before_params = &trimmed[..first_paren];
                                if before_params.contains("(*") {
                                    continue;
                                }
                            }
                            // Bug74 fix: Extract function name from prototype line
                            // Handle __attribute__((...)) before function name by stripping it first
                            let mut search_str = trimmed;

                            // Strip __attribute__((...)) prefix if present
                            while let Some(attr_pos) = search_str.find("__attribute__") {
                                let after_attr = &search_str[attr_pos + 13..];
                                if after_attr.starts_with("((") {
                                    if let Some(close_pos) = after_attr.find("))") {
                                        search_str = &search_str[attr_pos + 13 + close_pos + 2..].trim_start();
                                    } else {
                                        break;
                                    }
                                } else if after_attr.starts_with("(") {
                                    if let Some(close_pos) = after_attr.find(')') {
                                        search_str = &search_str[attr_pos + 13 + close_pos + 1..].trim_start();
                                    } else {
                                        break;
                                    }
                                } else {
                                    break;
                                }
                            }

                            if let Some(first_paren) = search_str.find('(') {
                                let before_paren = search_str[..first_paren].trim_end();

                                // Bug77 fix: Skip lines that are clearly NOT prototypes:
                                // 1. Contains '=' before the function name (assignment like "x = func(...);")
                                // 2. Contains '->' or '.' before the function name (method call)
                                // These are function CALLS, not prototype declarations.
                                if before_paren.contains('=') || before_paren.contains("->") || before_paren.contains('.') {
                                    continue;
                                }

                                // Extract function name: last word before (
                                let func_name = if let Some(last_space) = before_paren.rfind(|c: char| !c.is_alphanumeric() && c != '_') {
                                    &before_paren[last_space + 1..]
                                } else {
                                    // Bug77 fix: If there's no space/separator before the function name,
                                    // this is likely a function CALL (e.g., "sqlite3_result_error(...);"),
                                    // not a prototype. Prototypes always have a return type before the name.
                                    continue;
                                };

                                // Bug77 fix: Check if word before func_name is a C keyword (return, if, while, etc.)
                                // If so, this is a function call/control statement, not a prototype.
                                // Prototypes have type keywords (int, void, char, etc.) before the function name.
                                let before_func = before_paren[..before_paren.len() - func_name.len()].trim();
                                let last_word_before = before_func.rsplit(|c: char| !c.is_alphanumeric() && c != '_').next().unwrap_or("");
                                if matches!(last_word_before, "return" | "if" | "while" | "for" | "switch" | "case" | "sizeof" | "typeof" | "goto") {
                                    continue;
                                }

                                // Bug77 fix: A valid prototype must end with a type keyword or * (pointer).
                                // If before_func ends with an operator (+, -, *, /, etc.) or comma, it's
                                // part of an expression, not a prototype declaration.
                                // Also skip if before_func is empty (just "funcname(...);" is a call, not prototype).
                                if before_func.is_empty() {
                                    continue;
                                }
                                let last_char = before_func.chars().last().unwrap_or(' ');
                                // If the last char is an operator (except *), it's not a prototype
                                // Note: * is allowed for pointer return types (char*, void*, etc.)
                                if matches!(last_char, '+' | '-' | '/' | '(' | ',' | '<' | '>' | '!' | '&' | '|' | '^' | '%' | '[') {
                                    continue;
                                }

                                if !func_name.is_empty() && func_name != "__attribute__" {
                                    if let Some(last_close) = trimmed.rfind(");") {
                                        if let Some(func_start) = trimmed.find(func_name) {
                                            if let Some(paren_pos) = trimmed[func_start..].find('(') {
                                                let params_start = func_start + paren_pos + 1;
                                                if params_start < last_close {
                                                    let params = trimmed[params_start..last_close].trim();
                                                    if !params.is_empty() {
                                                        early_embedded_prototypes.insert(func_name.to_string());
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                // Build early K&R declarations for functions called from type code
                // Use K&R style (no parameters) to avoid type dependencies
                let mut early_type_decls = String::new();
                let mut added_decls: std::collections::HashSet<String> = std::collections::HashSet::new();

                for func_name in &type_embedded_calls {
                    // Bug74: Skip if there's an embedded full prototype
                    if early_embedded_prototypes.contains(func_name) {
                        continue;
                    }
                    // Bug74 fix: Skip if function has a prototype PU that will be output later
                    // This prevents K&R declarations from conflicting with full prototypes
                    if funcs_with_prototypes.contains(func_name) {
                        continue;
                    }
                    // Skip if already added
                    if added_decls.contains(func_name) {
                        continue;
                    }

                    // Check if this function is in tags and get its return type
                    if let Some(units) = tags.get(func_name) {
                        for unit in units.iter() {
                            let unit_type = PuType::from_key(unit);
                            // Use efficient parser instead of split().collect()
                            let full_key = if unit_type == PuType::Prototype {
                                parse_key_type_rest(unit).map(|(_, rest)| format!("prototype:{}:{}", func_name, rest))
                            } else if unit_type == PuType::Function {
                                parse_key_type_rest(unit).map(|(_, rest)| format!("function:{}:{}", func_name, rest))
                            } else {
                                None
                            };

                            if let Some(key) = full_key {
                                if let Some(code) = pu.get(&key) {
                                    // Bug74 fix: Extract ONLY the return type, not the entire code.
                                    // Previously we used trimmed.contains("void *") which matched
                                    // "void *" in parameters like "void *pAux", not just return type.
                                    // Use generate_minimal_forward_decl to properly extract return type.
                                    if let Some(decl) = generate_minimal_forward_decl(func_name, code, None) {
                                        early_type_decls.push_str(&decl);
                                        early_type_decls.push('\n');
                                        added_decls.insert(func_name.clone());
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }

                // Write early K&R declarations BEFORE Pass 1 types
                if !early_type_decls.is_empty() {
                    buffered.append_raw("\n// Bug34: Early K&R declarations for functions called from type code\n");
                    buffered.append_raw(&early_type_decls);
                    buffered.append_raw("\n");
                }

                // Bug47 fix: Pass 0 - Output forward struct/union declarations BEFORE Pass 1
                // ctags captures "struct foo;" as externvar, but these need to be output before
                // any struct that references "struct foo *" in its members
                // OPTIMIZATION: Iterate over necessary_sorted instead of pu_order
                for u in necessary_sorted.iter() {
                    if PuType::from_key(u) != PuType::ExternVar {
                        continue;
                    }
                    if common_deps.contains(*u) {
                        continue;
                    }
                    let code = pu.get(*u).unwrap_or(&String::new()).clone();
                    // Skip preprocessor lines to find actual code
                    let actual_code: String = code.lines()
                        .filter(|line| !line.trim().starts_with('#'))
                        .collect::<Vec<_>>()
                        .join("\n");
                    let trimmed_code = actual_code.trim();
                    // Check if this externvar is actually a forward struct/union declaration
                    if (trimmed_code.starts_with("struct ") || trimmed_code.starts_with("union "))
                        && trimmed_code.ends_with(";")
                        && !trimmed_code.contains('{')
                    {
                        buffered.append(&code);
                    }
                }

                // Pass 1a: Output typedefs, enums, structs, unions (NOT variables yet)
                pass1_ran = true; // Mark that Pass 1 is running
                // OPTIMIZATION: Iterate over necessary_sorted (already excludes enumerators)
                for u in necessary_sorted.iter() {
                    if common_deps.contains(*u) {
                        continue;
                    }
                    let pu_type = PuType::from_key(u);

                    // Only output type definitions (not variables) in this sub-pass
                    // Bug32 fix: Prototypes are now output earlier in a separate block
                    if pu_type != PuType::Typedef && pu_type != PuType::Enum
                        && pu_type != PuType::Struct && pu_type != PuType::Union {
                        continue;
                    }

                    let code = pu.get(*u).unwrap_or(&String::new()).clone();
                    let trimmed_code = code.trim();
                    if trimmed_code.is_empty()
                        || trimmed_code == "enum"
                        || trimmed_code == "enum;"
                        || trimmed_code == "enum {}"
                        || trimmed_code == "struct;"
                        || trimmed_code == "union;"
                        || (trimmed_code.starts_with("#") && !trimmed_code.contains("\n"))
                    {
                        continue;
                    }

                    // Bug30 fix: Skip typedef unions/structs that reference internal glibc structs
                    // Bug36 fix: Only check the body, not the struct name itself
                    // Bug46 fix: Don't skip self-referential structs (typedef struct __X { struct __X *ptr; } Y;)
                    if (pu_type == PuType::Typedef || pu_type == PuType::Union || pu_type == PuType::Struct)
                        && trimmed_code.contains('{')
                    {
                        if let Some(brace_pos) = trimmed_code.find('{') {
                            let body = &trimmed_code[brace_pos..];
                            // Bug46: Extract the struct/union name being defined to check for self-references
                            let defining_name = extract_defining_struct_name(trimmed_code);
                            if has_external_internal_struct_ref(body, defining_name.as_deref()) {
                                if std::env::var("DEBUG_BUG30").is_ok() {
                                    eprintln!("DEBUG Bug30: Skipping {} due to internal glibc struct in body: {:?}...", *u, &body.chars().take(100).collect::<String>());
                                }
                                continue;
                            }
                        }
                    }

                    // Bug33 fix: Filter out K&R declarations if full prototypes exist in same code block
                    let filtered_code = filter_conflicting_knr_declarations(&code);
                    // Dedup: skip if an identical body was already emitted (e.g., struct + typedef same block)
                    let dedup_key = filtered_code.trim().to_string();
                    if !pass1_seen_bodies.contains(&dedup_key) {
                        pass1_seen_bodies.insert(dedup_key);
                        if std::env::var("DEBUG_PASS1").is_ok() && uid == 103 {
                            eprintln!("DEBUG Pass1a uid=103: key={} | type={:?} | body={:?}", *u, pu_type, filtered_code.trim().chars().take(100).collect::<String>());
                        }
                        buffered.append(&filtered_code);
                    }
                }

                // Bug33 fix: Output proto_decls AFTER Pass 1a (typedefs now defined)
                // This ensures type names like Schema, sqlite3 are defined before declarations use them
                if !proto_decls.is_empty() {
                    buffered.append_raw(&proto_decls);
                }

                // Bug72: Output extern declarations AFTER Pass 1a (typedefs now defined)
                if !extern_func_decls_output.is_empty() {
                    buffered.append_raw(&extern_func_decls_output);
                }
                if !extern_var_decls_output.is_empty() {
                    buffered.append_raw(&extern_var_decls_output);
                }

                // Pass 2: Write K&R forward declarations BEFORE variables
                // This ensures variables with function-pointer initializers (e.g., nfa_regengine)
                // have forward declarations available before the variable definition
                buffered.append_raw(&forward_decls);

                // Pass 1b: Output variables (AFTER forward decls so fn-ptr initializers resolve)
                for u in necessary_sorted.iter() {
                    if common_deps.contains(*u) {
                        continue;
                    }
                    let pu_type = PuType::from_key(u);

                    if pu_type != PuType::Variable {
                        continue;
                    }

                    let code = pu.get(*u).unwrap_or(&String::new()).clone();
                    let trimmed_code = code.trim();
                    if trimmed_code.is_empty()
                        || (trimmed_code.starts_with("#") && !trimmed_code.contains("\n"))
                    {
                        continue;
                    }

                    // Bug33 fix: Filter out K&R declarations if full prototypes exist in same code block
                    let filtered_code = filter_conflicting_knr_declarations(&code);
                    // Dedup: skip if an identical body was already emitted
                    let dedup_key = filtered_code.trim().to_string();
                    if !pass1_seen_bodies.contains(&dedup_key) {
                        pass1_seen_bodies.insert(dedup_key);
                        if std::env::var("DEBUG_PASS1").is_ok() && uid == 103 {
                            eprintln!("DEBUG Pass1b uid=103: key={} | type={:?} | body={:?}", *u, pu_type, filtered_code.trim().chars().take(100).collect::<String>());
                        }
                        buffered.append(&filtered_code);
                    }
                }
            }
        }

        // Bug33 fix: Also output proto_decls when forward_decl_funcs was empty
        // (Pass 1 ran via the fallback path below)
        // Note: This is outside the if !forward_decl_funcs.is_empty() block

        // Bug36 fix: Pass 1 (struct/typedef output) should run UNCONDITIONALLY,
        // even if there are no forward function declarations needed.
        // The old code only ran Pass 1 when forward_decl_funcs was not empty.
        // This caused structs like __jmp_buf_tag to be missing from output.
        if !pass1_ran {
            pass1_ran = true;

            // Bug47 fix: Pass 0 - Output forward struct/union declarations BEFORE Pass 1
            // ctags captures "struct foo;" as externvar, but these need to be output before
            // any struct that references "struct foo *" in its members
            // OPTIMIZATION: Iterate over necessary_sorted instead of pu_order
            for u in necessary_sorted.iter() {
                if PuType::from_key(u) != PuType::ExternVar {
                    continue;
                }
                if common_deps.contains(*u) {
                    continue;
                }
                let code = pu.get(*u).unwrap_or(&String::new()).clone();
                // Skip preprocessor lines to find actual code
                let actual_code: String = code.lines()
                    .filter(|line| !line.trim().starts_with('#'))
                    .collect::<Vec<_>>()
                    .join("\n");
                let trimmed_code = actual_code.trim();
                // Check if this externvar is actually a forward struct/union declaration
                if (trimmed_code.starts_with("struct ") || trimmed_code.starts_with("union "))
                    && trimmed_code.ends_with(";")
                    && !trimmed_code.contains('{')
                {
                    buffered.append(&code);
                }
            }

            // OPTIMIZATION: Iterate over necessary_sorted instead of pu_order
            for u in necessary_sorted.iter() {
                // necessary_sorted already excludes enumerators
                if common_deps.contains(*u) {
                    continue;
                }
                let pu_type = PuType::from_key(u);

                // Only output type definitions and variables in this pass
                if pu_type != PuType::Typedef && pu_type != PuType::Enum
                    && pu_type != PuType::Struct && pu_type != PuType::Union
                    && pu_type != PuType::Variable {
                    continue;
                }
                let code = pu.get(*u).unwrap_or(&String::new()).clone();
                let trimmed_code = code.trim();
                if trimmed_code.is_empty()
                    || trimmed_code == "enum"
                    || trimmed_code == "enum;"
                    || trimmed_code == "enum {}"
                    || trimmed_code == "struct;"
                    || trimmed_code == "union;"
                    || (trimmed_code.starts_with("#") && !trimmed_code.contains("\n"))
                {
                    continue;
                }

                // Bug30 fix: Skip typedef unions/structs that reference internal glibc structs
                // Bug36 fix: Don't skip structs named with __ prefix - only skip if they
                // REFERENCE another struct/union with __ prefix inside their body.
                // Check: the struct/union __ pattern must appear INSIDE the braces, not as the name.
                // Bug46 fix: Don't skip self-referential structs (typedef struct __X { struct __X *ptr; } Y;)
                if (pu_type == PuType::Typedef || pu_type == PuType::Union || pu_type == PuType::Struct)
                    && trimmed_code.contains('{')
                {
                    // Find the body (content after first '{')
                    if let Some(brace_pos) = trimmed_code.find('{') {
                        let body = &trimmed_code[brace_pos..];
                        // Bug46: Extract the struct/union name being defined to check for self-references
                        let defining_name = extract_defining_struct_name(trimmed_code);
                        if has_external_internal_struct_ref(body, defining_name.as_deref()) {
                            continue;
                        }
                    }
                }

                // Bug33 fix: Filter out K&R declarations if full prototypes exist in same code block
                let filtered_code = filter_conflicting_knr_declarations(&code);
                // Dedup: skip if an identical body was already emitted (e.g., struct + typedef same block)
                let dedup_key = filtered_code.trim().to_string();
                if !pass1_seen_bodies.contains(&dedup_key) {
                    pass1_seen_bodies.insert(dedup_key);
                    buffered.append(&filtered_code);
                }
            }

            // Bug33 fix: Output proto_decls AFTER Pass 1 (typedefs now defined)
            if !proto_decls.is_empty() {
                buffered.append_raw(&proto_decls);
            }

            // Bug72: Output extern declarations AFTER Pass 1 (typedefs now defined)
            if !extern_func_decls_output.is_empty() {
                buffered.append_raw(&extern_func_decls_output);
            }
            if !extern_var_decls_output.is_empty() {
                buffered.append_raw(&extern_var_decls_output);
            }
        }
    }

    // Handle no-split mode differently: output types first, then forward decls, then functions
    if !is_split_mode && c > 0 {
        // OPTIMIZATION: Use necessary_sorted.first() instead of searching pu_order
        let file_str = necessary_sorted.first()
            .and_then(|u| {
                let a: Vec<&str> = u.split(":").collect();
                if a.len() >= 3 { Some(a[2].to_owned()) } else { None }
            })
            .unwrap_or_default();

        if !file_str.is_empty() {
            // Bug47 fix: Pass 0 - Output forward struct/union declarations BEFORE Pass 1
            // ctags captures "struct foo;" as externvar, but these need to be output before
            // any struct that references "struct foo *" in its members
            // OPTIMIZATION: Iterate over necessary_sorted instead of pu_order
            for u in necessary_sorted.iter() {
                if PuType::from_key(u) != PuType::ExternVar {
                    continue;
                }
                if common_deps.contains(*u) {
                    continue;
                }
                let code = pu.get(*u).unwrap_or(&String::new()).clone();
                // Skip preprocessor lines to find actual code
                let actual_code: String = code.lines()
                    .filter(|line| !line.trim().starts_with('#'))
                    .collect::<Vec<_>>()
                    .join("\n");
                let trimmed_code = actual_code.trim();
                // Check if this externvar is actually a forward struct/union declaration
                if (trimmed_code.starts_with("struct ") || trimmed_code.starts_with("union "))
                    && trimmed_code.ends_with(";")
                    && !trimmed_code.contains('{')
                {
                    buffered.append(&code);
                }
            }

            // Pass 1: Output all non-function units (typedefs, enums, structs, variables, externs)
            // OPTIMIZATION: Iterate over necessary_sorted instead of pu_order
            for u in necessary_sorted.iter() {
                // necessary_sorted already excludes enumerators
                if common_deps.contains(*u) {
                    continue;
                }

                let pu_type = PuType::from_key(u);

                // Skip functions in first pass
                if pu_type == PuType::Function {
                    continue;
                }

                let code = pu.get(*u).unwrap_or(&String::new()).clone();
                let trimmed_code = code.trim();
                if trimmed_code.is_empty()
                    || trimmed_code == "enum"
                    || trimmed_code == "enum;"
                    || trimmed_code == "enum {}"
                    || trimmed_code == "struct;"
                    || trimmed_code == "union;"
                    || (trimmed_code.starts_with("#") && !trimmed_code.contains("\n"))
                {
                    continue;
                }

                buffered.append(&code);
            }

            // Pass 2: Output forward declarations for all functions
            // OPTIMIZATION: Iterate over necessary_sorted instead of pu_order
            let mut forward_decls = String::new();
            forward_decls.push_str("\n// Forward declarations\n");
            for u in necessary_sorted.iter() {
                // necessary_sorted already excludes enumerators
                let pu_type = PuType::from_key(u);

                if pu_type == PuType::Function {
                    if let Some(code) = pu.get(*u) {
                        let decl = convert_function_to_declaration(code);
                        if !decl.trim().is_empty() && !decl.contains("static ") {
                            forward_decls.push_str(&decl);
                            forward_decls.push('\n');
                        }
                    }
                }
            }
            forward_decls.push_str("\n// End of forward declarations\n\n");
            buffered.append_raw(&forward_decls);

            // Bug71: Add static function pointer variable declarations that ctags doesn't capture
            // Output after typedefs/Pass2 so types are defined
            if !static_funcptr_vars.is_empty() {
                let all_code_identifiers: FxHashSet<&str> = code_identifiers.get_union(necessary.iter());
                let mut referenced_static_funcptrs: Vec<&str> = static_funcptr_vars.keys()
                    .filter(|var_name| {
                        let name_str = var_name.as_str();
                        if !all_code_identifiers.contains(name_str) {
                            return false;
                        }
                        let var_key_pattern = format!("variable:{}", name_str);
                        if necessary.iter().any(|k| k.contains(&var_key_pattern)) {
                            return false;
                        }
                        true
                    })
                    .map(|s| s.as_str())
                    .collect();
                referenced_static_funcptrs.sort();

                if !referenced_static_funcptrs.is_empty() {
                    let static_funcptr_decls: String = referenced_static_funcptrs.iter()
                        .filter_map(|name| static_funcptr_vars.get(*name))
                        .cloned()
                        .collect::<Vec<_>>()
                        .join("\n");

                    buffered.append_raw("// Static function pointer variable declarations (not captured by ctags)\n");
                    buffered.append_raw(&static_funcptr_decls);
                    buffered.append_raw("\n");
                }
            }

            // Pass 3: Output all function definitions
            // OPTIMIZATION: Iterate over necessary_sorted instead of pu_order
            for u in necessary_sorted.iter() {
                // necessary_sorted already excludes enumerators
                if common_deps.contains(*u) {
                    continue;
                }

                let pu_type = PuType::from_key(u);

                // Only functions in third pass
                if pu_type != PuType::Function {
                    continue;
                }

                let code = pu.get(*u).unwrap_or(&String::new()).clone();
                let trimmed_code = code.trim();
                if trimmed_code.is_empty() {
                    continue;
                }

                buffered.append(&code);
            }
        }
    } else {
        // Split mode or no necessary units: original logic

        // Bug71: Add static function pointer variable declarations that ctags doesn't capture
        // This needs to be output after typedefs but before functions in split mode
        // For split mode, we track whether to output after typedefs are done
        let mut static_funcptr_vars_output = false;

        // Pass 3: write functions, variables, and remaining declarations
        // (typedefs, enums, structs, unions were already written in Pass 1)
        // OPTIMIZATION: Iterate over necessary_sorted instead of pu_order
        for u in necessary_sorted.iter() {
            // necessary_sorted already excludes enumerators
            // Skip if this declaration is in the common header
            if common_deps.contains(*u) {
                continue;
            }

            let pu_type = PuType::from_key(u);

            // Bug25 fix: Skip types that were already output in Pass 1
            // (typedefs, enums, structs, unions)
            // Bug29 fix: Only skip if Pass 1 actually ran (it doesn't run when no forward decls needed)
            // Bug32 fix: Also skip prototypes (they were output in Pass 1 to ensure proper ordering)
            // Also skip Variables since they are now output in Pass 1 of split mode
            if pass1_ran && (pu_type == PuType::Typedef || pu_type == PuType::Enum
                || pu_type == PuType::Struct || pu_type == PuType::Union
                || pu_type == PuType::Prototype || pu_type == PuType::Variable) {
                continue;
            }

            let mut code = pu.get(*u).unwrap_or(&String::new()).clone();

            // Skip empty or trivial declarations that would produce invalid C
            // e.g., "enum;" or "enum {}" or just whitespace/line markers
            let trimmed_code = code.trim();
            if trimmed_code.is_empty()
                || trimmed_code == "enum"
                || trimmed_code == "enum;"
                || trimmed_code == "enum {}"
                || trimmed_code == "struct;"
                || trimmed_code == "union;"
                || (trimmed_code.starts_with("#") && !trimmed_code.contains("\n"))  // single line markers
            {
                continue;
            }

            // Bug30 fix: Skip typedef unions/structs that reference internal glibc structs
            // Bug36 fix: Only check the body, not the struct name itself
            if (pu_type == PuType::Typedef || pu_type == PuType::Union || pu_type == PuType::Struct)
                && trimmed_code.contains('{')
            {
                if let Some(brace_pos) = trimmed_code.find('{') {
                    let body = &trimmed_code[brace_pos..];
                    if body.contains("struct __") || body.contains("union __") {
                        continue;
                    }
                }
            }

            // Bug71: Output static function pointer variable declarations before first function
            // This ensures typedefs have been output (they come before functions in pu_order)
            if !static_funcptr_vars_output && pu_type == PuType::Function {
                static_funcptr_vars_output = true;
                if !static_funcptr_vars.is_empty() {
                    let all_code_identifiers: FxHashSet<&str> = code_identifiers.get_union(necessary.iter());
                    let mut referenced_static_funcptrs: Vec<&str> = static_funcptr_vars.keys()
                        .filter(|var_name| {
                            let name_str = var_name.as_str();
                            if !all_code_identifiers.contains(name_str) {
                                return false;
                            }
                            let var_key_pattern = format!("variable:{}", name_str);
                            if necessary.iter().any(|k| k.contains(&var_key_pattern)) {
                                return false;
                            }
                            true
                        })
                        .map(|s| s.as_str())
                        .collect();
                    referenced_static_funcptrs.sort();

                    if !referenced_static_funcptrs.is_empty() {
                        let static_funcptr_decls: String = referenced_static_funcptrs.iter()
                            .filter_map(|name| static_funcptr_vars.get(*name))
                            .cloned()
                            .collect::<Vec<_>>()
                            .join("\n");

                        buffered.append_raw("// Static function pointer variable declarations (not captured by ctags)\n");
                        buffered.append_raw(&static_funcptr_decls);
                        buffered.append_raw("\n");
                    }
                }
            }

            k += 1;
            // Extract func_name from key using parse_key_parts
            let func_name = extract_key_name(*u).unwrap_or("");

            // Convert dependencies to declarations in split mode only
            // In no-split mode, keep all function/variable bodies
            if is_split_mode {
                // Chunked mode: use primary_functions to determine what keeps full body
                // Standard split mode: use target_function to identify the primary unit
                // Bug68 fix: Changed from k >= c to target_function check because Pass 1
                // skips types (typedef/enum/struct/union/prototype), so c counts more
                // units than k ever reaches, making is_primary always false
                let is_primary = match primary_functions {
                    Some(pf) => pf.contains(*u),
                    None => target_function.map_or(k >= c, |tf| *u == tf),
                };
                if std::env::var("DEBUG_SPLIT_PASS3").is_ok() && uid == 103 {
                    eprintln!("DEBUG SPLIT_PASS3 uid=103 {}: is_primary={} pu_type={:?} code_start={:?}", func_name, is_primary, pu_type, code.trim().chars().take(60).collect::<String>());
                }

                // In chunked mode, skip non-primary functions entirely
                // (they were already output as forward declarations earlier)
                if primary_functions.is_some() && pu_type == PuType::Function && !is_primary {
                    continue;
                }

                // Convert dependencies to declarations, keep primary as full implementation
                if pu_type == PuType::Variable && !is_primary {
                    code = convert_variable_to_declaration(&code);
                }
                if pu_type == PuType::Function && !is_primary {
                    // Skip writing full-signature declaration if this function already has
                    // a K&R forward declaration - writing both causes "conflicting types" errors
                    if funcs_with_forward_decl.contains(func_name) {
                        continue;
                    }
                    // Bug32 fix: Skip functions that were already output early as declarations
                    // (functions returning void*, char*, or other pointer types)
                    if early_output_funcs.contains(*u) {
                        continue;
                    }
                    // Bug67 fix: Use convert_function_to_declaration_with_name to handle
                    // cases where ctags captures multiple functions in the same code span
                    code = convert_function_to_declaration_with_name(&code, func_name);
                }
                // Also skip prototypes for functions with K&R forward declarations
                if pu_type == PuType::Prototype && funcs_with_forward_decl.contains(func_name) {
                    continue;
                }
            }
            // code.push('\n');
            // Always write to file (k <= c is always true since k increments up to c)
            buffered.append(&code);
        }
    }

    // Final flush: write all buffered content to file in a single write
    if DEBUG_TAGS_ENABLED.load(Ordering::Relaxed) {
        eprintln!("DEBUG flush_to_file: output_file={} buffer_len={}", output_file, buffered.buffer.len());
    }
    buffered.flush_to_file(&output_file).unwrap();
}

#[allow(dead_code)]
#[inline(always)]
fn convert_variable_to_declaration(code: &str) -> String {
    let mut code = code.to_string();
    // Preserve type definitions (e.g., typedef unsigned int wchar_t;)
    if code.trim_start().starts_with("typedef") {
        return code;
    }
    if code.trim().starts_with("#") {
        return code;
    }

    // Check for arrays with unsized brackets [] and initializers
    // These cannot be converted to declarations because sizeof() would fail
    // Pattern: something[] = { ... } or something[] = "..."
    if code.contains("[]") && code.contains("=") {
        // Check for brace initializer or string initializer after the '='
        if let Some(eq_pos) = code.find('=') {
            let after_eq = code[eq_pos + 1..].trim_start();
            if after_eq.starts_with('{') || after_eq.starts_with('"') {
                // Keep the full definition - cannot be converted to forward declaration
                return code;
            }
        }
    }

    if code.contains("#pragma") {
        let x: Vec<&str> = code.split("\n").collect();
        if x.len() > 2 {
            let mut new_code = format!("{} {} \nextern ", x[1], x[2]);
            for xi in x.iter().skip(3) {
                new_code = format!("{} {}", new_code, xi);
            }
            code = new_code;
        }
    } else {
        if !code.contains("extern ") && !code.contains("static ") && !code.trim().starts_with("typedef") {
            let trimmed = code.trim_start();
            // Check if this is a comma-continuation variable (no type specifier)
            // These look like " varname;" or "varname;" with no type keyword
            // Common type-like prefixes that indicate this is a proper declaration
            let has_type = trimmed.starts_with("const ")
                || trimmed.starts_with("volatile ")
                || trimmed.starts_with("register ")
                || trimmed.starts_with("_Thread_local ")
                || trimmed.starts_with("__thread ")
                || trimmed.starts_with("unsigned ")
                || trimmed.starts_with("signed ")
                || trimmed.starts_with("long ")
                || trimmed.starts_with("short ")
                || trimmed.starts_with("int ")
                || trimmed.starts_with("char ")
                || trimmed.starts_with("float ")
                || trimmed.starts_with("double ")
                || trimmed.starts_with("void ")
                || trimmed.starts_with("_Bool ")
                || trimmed.starts_with("struct ")
                || trimmed.starts_with("union ")
                || trimmed.starts_with("enum ")
                || trimmed.contains(" ")  // Has at least type + name
                || trimmed.contains("*");  // Pointer type like "int*x"

            if has_type {
                let whitespace_len = code.len() - trimmed.len();
                let whitespace = &code[..whitespace_len];
                code = format!("{}extern {}", whitespace, trimmed);
            }
            // Note: If no type (comma-continuation), don't add extern, just return as-is
            // These should be empty strings now since they're merged with the previous variable
        }
    }
    let a: Vec<&str> = code.split("=").collect();
    if a.len() > 1 {
        code = format!("{};", a[0]);
    }

    // Handle comma-separated variable declarations like "static long x, y;"
    // If the code ends with a comma (first var in comma-separated decl),
    // replace the trailing comma with semicolon to make it a complete declaration
    let trimmed_end = code.trim_end();
    if trimmed_end.ends_with(',') {
        code = format!("{};", &trimmed_end[..trimmed_end.len()-1]);
    }

    code
}

/// Bug33 fix: Filter out K&R prototype declarations from code blocks when a full prototype exists.
///
/// A K&R declaration looks like: `static void *get_schema();`
/// A full prototype looks like: `static Schema *get_schema(sqlite3 *db, Btree *pBt);`
///
/// This function scans the code for prototype declarations, identifies K&R ones (empty parens),
/// and removes them if a full prototype exists for the same function name.
fn filter_conflicting_knr_declarations(code: &str) -> String {
    use std::collections::{HashMap, HashSet};

    let lines: Vec<&str> = code.lines().collect();

    // First pass: identify all function declarations and whether they're K&R or full
    // Map from function name -> (K&R line indices, full prototype line indices)
    let mut func_decls: HashMap<String, (Vec<usize>, Vec<usize>)> = HashMap::new();

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();

        // Look for prototype declarations (ending with );)
        if !trimmed.ends_with(");") {
            continue;
        }

        // Skip lines that look like function pointer typedefs
        if trimmed.contains("typedef") || trimmed.contains("(*") {
            continue;
        }

        // Find the function name (word before the opening paren)
        if let Some(paren_pos) = trimmed.rfind('(') {
            // Extract what's before the paren
            let before_paren = &trimmed[..paren_pos].trim_end();
            // Find the last word (function name)
            if let Some(last_space) = before_paren.rfind(|c: char| !c.is_alphanumeric() && c != '_') {
                let func_name = &before_paren[last_space + 1..];
                if !func_name.is_empty() {
                    // Check if this is K&R (empty parens or just void)
                    if let Some(close_paren) = trimmed[paren_pos..].find(')') {
                        let params = trimmed[paren_pos + 1..paren_pos + close_paren].trim();
                        let is_knr = params.is_empty() || params == "void";

                        let entry = func_decls.entry(func_name.to_string()).or_insert_with(|| (Vec::new(), Vec::new()));
                        if is_knr {
                            entry.0.push(i);
                        } else {
                            entry.1.push(i);
                        }
                    }
                }
            }
        }
    }

    // Second pass: collect line indices to skip (K&R lines where full prototype exists)
    let mut skip_lines: HashSet<usize> = HashSet::new();
    for (_func_name, (knr_lines, full_lines)) in func_decls.iter() {
        if !full_lines.is_empty() {
            // Full prototype exists, skip all K&R lines for this function
            for line_idx in knr_lines {
                skip_lines.insert(*line_idx);
            }
        }
    }

    // Third pass: rebuild the code without the skipped lines
    if skip_lines.is_empty() {
        return code.to_string();
    }

    let filtered: Vec<&str> = lines.iter().enumerate()
        .filter(|(i, _)| !skip_lines.contains(i))
        .map(|(_, line)| *line)
        .collect();

    filtered.join("\n")
}

#[allow(dead_code)]
#[inline(always)]
/// Convert function code to a forward declaration.
/// Bug67 fix: Takes func_name to handle cases where ctags captures multiple functions
/// in the same code span (e.g., unixDlSym + unixDlClose captured together).
/// Returns declarations for ALL functions in the code span, not just the target.
fn convert_function_to_declaration_with_name(code: &str, _func_name: &str) -> String {
    // Bug67 fix: If the code contains multiple function definitions,
    // we need to extract declarations for ALL of them, not just the target.
    // This handles ctags bug where adjacent functions are captured together.

    // Find all function definitions by looking for patterns like:
    // "static type funcname(" or "type funcname(" followed by "{"
    let mut declarations: Vec<String> = Vec::new();
    let mut search_pos = 0;

    while search_pos < code.len() {
        // Find the next opening brace
        let brace_pos = match code[search_pos..].find('{') {
            Some(pos) => search_pos + pos,
            None => break,
        };

        // Find the start of this function's signature (look backwards for newline from brace position)
        // But only look within the region from search_pos to brace_pos
        let sig_start = code[search_pos..brace_pos].rfind('\n')
            .map(|p| search_pos + p + 1)
            .unwrap_or(search_pos);

        // Skip preprocessor lines (they start with #)
        let sig_line = code[sig_start..brace_pos].trim();
        if sig_line.starts_with('#') {
            search_pos = brace_pos + 1;
            continue;
        }

        // Extract the signature and generate declaration
        if !sig_line.is_empty() {
            // Make sure it looks like a function signature (contains '(' before the end)
            if sig_line.contains('(') {
                // Bug73: Strip always_inline from declarations since they don't have bodies
                let mut decl = sig_line.to_string();
                decl = decl.replace("__attribute__ ((__always_inline__))", "");
                decl = decl.replace("__attribute__((__always_inline__))", "");
                decl = decl.replace("__attribute__((always_inline))", "");
                decl = decl.replace("__attribute__ ((always_inline))", "");
                declarations.push(format!("{};", decl.trim()));
            }
        }

        // Find the matching closing brace to skip the function body
        // Bug68 fix: Must skip character literals ('x'), string literals ("..."), and comments
        // to avoid counting braces inside them (e.g., "if (*p == '}')" has a } in char literal)
        let mut brace_depth = 1;
        let mut pos = brace_pos + 1;
        let bytes = code.as_bytes();
        while pos < bytes.len() && brace_depth > 0 {
            match bytes[pos] {
                b'\'' => {
                    // Skip character literal - look for closing quote
                    pos += 1;
                    while pos < bytes.len() {
                        if bytes[pos] == b'\\' && pos + 1 < bytes.len() {
                            pos += 2; // Skip escaped char
                        } else if bytes[pos] == b'\'' {
                            pos += 1;
                            break;
                        } else {
                            pos += 1;
                        }
                    }
                }
                b'"' => {
                    // Skip string literal - look for closing quote
                    pos += 1;
                    while pos < bytes.len() {
                        if bytes[pos] == b'\\' && pos + 1 < bytes.len() {
                            pos += 2; // Skip escaped char
                        } else if bytes[pos] == b'"' {
                            pos += 1;
                            break;
                        } else {
                            pos += 1;
                        }
                    }
                }
                b'/' if pos + 1 < bytes.len() => {
                    if bytes[pos + 1] == b'/' {
                        // Skip line comment - look for newline
                        pos += 2;
                        while pos < bytes.len() && bytes[pos] != b'\n' {
                            pos += 1;
                        }
                    } else if bytes[pos + 1] == b'*' {
                        // Skip block comment - look for */
                        pos += 2;
                        while pos + 1 < bytes.len() {
                            if bytes[pos] == b'*' && bytes[pos + 1] == b'/' {
                                pos += 2;
                                break;
                            }
                            pos += 1;
                        }
                    } else {
                        pos += 1;
                    }
                }
                b'{' => {
                    brace_depth += 1;
                    pos += 1;
                }
                b'}' => {
                    brace_depth -= 1;
                    pos += 1;
                }
                _ => {
                    pos += 1;
                }
            }
        }
        search_pos = pos;
    }

    // If we found multiple declarations, return all of them
    if declarations.len() > 1 {
        return declarations.join("\n");
    } else if declarations.len() == 1 {
        return declarations[0].clone();
    }

    // Fall back to original behavior
    convert_function_to_declaration(code)
}

fn convert_function_to_declaration(code: &str) -> String {
    let mut code = code.to_string();
    // Preserve type definitions (typedef ...) and avoid adding extern to them
    if code.trim_start().starts_with("typedef") {
        return code;
    }
    code = code.replace("__attribute__ ((__always_inline__))", "");
    code = code.replace("__attribute__((__always_inline__))", "");
    // Bug73: Also handle `always_inline` without double underscores (sqlite3 style)
    code = code.replace("__attribute__((always_inline))", "");
    code = code.replace("__attribute__ ((always_inline))", "");
    code = code.replace("__always_inline__", "");
    code = code.replace("__attribute__ ((__gnu_inline__))", "");
    code = code.replace("__attribute__((__gnu_inline__))", "");
    code = code.replace("__gnu_inline__", "");
    code = code.replace("__attribute__ ((__artificial__))", "");
    code = code.replace("__attribute__ ((__artificial__))", "");
    code = code.replace("__attribute__((__artificial__))", "");
    code = code.replace("__inline__", "");
    code = code.replace("__inline", "");
    code = code.replace("inline ", "");
    if !code.contains("extern ") && !code.contains("static ") && !code.contains("#") {
        let trimmed = code.trim_start();
        let whitespace_len = code.len() - trimmed.len();
        let whitespace = &code[..whitespace_len];
        code = format!("{}extern {}", whitespace, trimmed);
    }
    // If code already doesn't have a body (no '{'), it might already be a declaration
    if !code.contains("{") {
        // Check if it already has a semicolon
        let trimmed = code.trim_end();
        if trimmed.ends_with(";") {
            return code; // Already a declaration
        }
        // Add semicolon, removing any trailing newlines first
        code = format!("{};", trimmed);
    } else {
        // Extract everything before the opening brace
        let a: Vec<&str> = code.split("{").collect();
        // Remove trailing whitespace before adding semicolon
        code = format!("{};", a[0].trim_end());
    }
    code
}

/// Generate a minimal K&R-style forward declaration for a function.
/// This only outputs `static <name>();` or `<name>();` without parameter types,
/// which works for function pointers where types might not be defined yet.
///
/// `available_types` is an optional set of type names (typedefs, structs) that are
/// available in the current PU. If the return type is in this set, we can use the
/// actual return type instead of falling back to void*.
///
/// Returns None if unable to extract a valid function name.
fn generate_minimal_forward_decl(func_name: &str, func_code: &str, available_types: Option<&std::collections::HashSet<String>>) -> Option<String> {
    let trimmed = func_code.trim();

    // Find the function signature line containing the function name
    // We need to extract: [static] <return_type> <func_name>(
    // Skip preprocessor directives (# lines)
    // Bug61 fix: Handle vim K&R style where return type is on separate line:
    //     void
    // func_name(...)
    let mut signature_prefix = String::new();
    let mut prev_line = String::new();  // Track previous non-preprocessor line

    for line in trimmed.lines().take(10) {
        let l = line.trim();
        // Skip preprocessor lines
        if l.starts_with('#') {
            continue;
        }

        // Look for the function name followed by '(' - this is the signature line
        if let Some(name_pos) = l.find(func_name) {
            // Check if this is followed by '(' (it's a function definition/declaration)
            let after_name = &l[name_pos + func_name.len()..];
            if after_name.trim_start().starts_with('(') {
                // Extract everything before the function name as the return type prefix
                signature_prefix = l[..name_pos].trim().to_string();

                // Bug61: If prefix is empty, check the previous line for return type
                // This handles vim K&R style: "    void\nfunc_name(...)"
                if signature_prefix.is_empty() && !prev_line.is_empty() {
                    signature_prefix = prev_line.clone();
                }
                break;
            }
        }

        // Remember this line as the previous non-preprocessor line
        prev_line = l.to_string();
    }

    // If we couldn't find a signature, fall back to int
    if signature_prefix.is_empty() {
        return Some(format!("int {}();", func_name));
    }

    // Bug73: Strip always_inline attribute from forward declarations
    // always_inline requires the function body to be available for inlining,
    // but forward declarations don't have bodies, causing compiler errors like:
    // "error: inlining failed in call to 'always_inline' 'func': function body not available"
    // Use regex to strip __attribute__((always_inline)) variants
    let always_inline_re = regex::Regex::new(r#"__attribute__\s*\(\s*\(\s*always_inline\s*\)\s*\)"#).unwrap();
    let signature_prefix = always_inline_re.replace_all(&signature_prefix, "").to_string();
    let signature_prefix = signature_prefix.replace("  ", " ").trim().to_string();

    // Validate that the return type only contains basic C types to avoid undefined type errors
    // Basic types: void, int, char, short, long, float, double, unsigned, signed, const, static, extern, inline
    // Also allow pointer modifier '*'
    // Note: DO NOT include custom typedefs like i64, u64, size_t etc. - these may not be defined
    // in the PU file and will cause compilation errors
    let basic_types = ["void", "int", "char", "short", "long", "float", "double",
                       "unsigned", "signed", "const", "static", "extern", "inline"];

    // Split the prefix into words and check each word
    let words: Vec<&str> = signature_prefix.split_whitespace()
        .flat_map(|w| w.split('*'))
        .filter(|w| !w.is_empty())
        .collect();

    let is_basic_type = words.iter().all(|word| {
        basic_types.contains(word) || word.is_empty()
    });

    // Bug26 fix: If return type contains custom types (typedefs), we need to be careful.
    // The custom type may have transitive dependencies that are not available at the
    // point where the K&R forward declaration appears.
    // For example: `typedef base_int64 myint64; typedef myint64 i64;`
    // Using `i64` in a K&R decl requires both `myint64` and `base_int64` to be defined first.
    //
    // Bug69 fix: However, using void* breaks when the return value is dereferenced with
    // '*' or '->' operators (e.g., *ml_get((linenr_T)1)). If the typedef (like char_u)
    // is available in the current PU, use it instead of void*.
    if !is_basic_type {
        let is_static = signature_prefix.contains("static");
        let is_pointer = signature_prefix.contains('*');

        // Bug69: Check if the return type typedef is available in the PU
        // Extract the typedef name (first non-keyword word before '*')
        let type_words: Vec<&str> = signature_prefix.split_whitespace()
            .filter(|w| !["static", "extern", "const", "inline", "__inline", "__inline__"].contains(w))
            .collect();

        // Check if the first non-keyword word (the typedef) is available
        let can_use_typedef = if let Some(available) = available_types {
            type_words.first().map(|t| {
                // Strip any trailing '*' from the type name
                let clean_type = t.trim_end_matches('*');
                available.contains(clean_type)
            }).unwrap_or(false)
        } else {
            false
        };

        if can_use_typedef {
            // The typedef is available, use the actual signature prefix
            return Some(format!("{} {}();", signature_prefix, func_name));
        }

        // Fall back to void*/int for custom types to avoid transitive dependency issues
        if is_static {
            if is_pointer {
                return Some(format!("static void *{}();", func_name));
            } else {
                return Some(format!("static int {}();", func_name));
            }
        } else {
            if is_pointer {
                return Some(format!("void *{}();", func_name));
            } else {
                return Some(format!("int {}();", func_name));
            }
        }
    }

    // The signature_prefix now contains something like "static void *" or "int" or "void"
    // Generate the forward declaration using the extracted prefix
    Some(format!("{} {}();", signature_prefix, func_name))
}

#[allow(dead_code)]
#[inline(always)]
fn clear_file(file_path: &str) -> io::Result<()> {
    let _ = File::create(file_path)?;
    Ok(())
}

/// Strip CPP line directives from content.
/// Line directives like `# 123 "/path/to/file.c"` cause gcc to try to trace
/// header hierarchies, which fails for split .pu.c files since they're already
/// preprocessed and self-contained.
#[inline(always)]
fn strip_line_directives(content: &str) -> String {
    // Check if content ends with newline (we want to preserve it)
    let ends_with_newline = content.ends_with('\n');

    let result = content.lines()
        .filter(|line| {
            let trimmed = line.trim_start();
            // Keep lines that don't start with # followed by a number
            // Line directives have format: # linenum "filename" [flags]
            if trimmed.starts_with('#') {
                let rest = trimmed[1..].trim_start();
                // If it starts with a digit, it's a line directive - skip it
                !rest.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false)
            } else {
                true
            }
        })
        .collect::<Vec<&str>>()
        .join("\n");

    // Preserve trailing newline if original had one
    if ends_with_newline && !result.is_empty() {
        format!("{}\n", result)
    } else {
        result
    }
}

#[allow(dead_code)]
#[inline(always)]
fn write_to_file(content: &str, file_path: &str) -> io::Result<()> {
    let file = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(file_path)?;
    let mut out = BufWriter::new(file);
    // Strip line directives to make .pu.c files self-contained
    let clean_content = strip_line_directives(content);
    out.write_all(clean_content.as_bytes())?;
    Ok(())
}

// ============================================================================
// BufferedOutput - Optimization for file I/O
// Collects all output in memory, writes once at the end to avoid syscall overhead
// ============================================================================

/// Buffered output builder for PU files
/// Collects all content in memory and writes once at the end
/// Reduces ~10+ syscalls per PU to just 1
struct BufferedOutput {
    buffer: String,
}

impl BufferedOutput {
    /// Create a new buffered output with pre-allocated capacity
    #[inline]
    fn with_capacity(capacity: usize) -> Self {
        BufferedOutput {
            buffer: String::with_capacity(capacity),
        }
    }

    /// Append content, stripping line directives inline
    #[inline]
    fn append(&mut self, content: &str) {
        // Inline line directive stripping to avoid intermediate allocation
        for line in content.lines() {
            let trimmed = line.trim_start();
            // Skip line directives (# followed by digit)
            if trimmed.starts_with('#') {
                let rest = trimmed[1..].trim_start();
                if rest.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false) {
                    continue;
                }
            }
            self.buffer.push_str(line);
            self.buffer.push('\n');
        }
    }

    /// Append a raw string without line directive stripping
    /// Use for strings that are known to be clean (e.g., comments, forward decls)
    #[inline]
    fn append_raw(&mut self, content: &str) {
        self.buffer.push_str(content);
    }

    /// Flush buffer to file (single write)
    #[inline]
    fn flush_to_file(self, file_path: &str) -> io::Result<()> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        std::fs::write(file_path, self.buffer.as_bytes())
    }
}

use std::ffi::CStr;
use std::os::raw::c_char;

// FFI declarations for C-side character buffer (eliminates ~2M FFI calls per file)
#[link(name = "dctags")]
extern "C" {
    fn precc_get_buffer() -> *const c_char;
    fn precc_get_buffer_len() -> usize;
    fn precc_clear_buffer();
}

/// Append C-side buffer content directly to target String (zero-allocation fast path)
/// Returns true if any data was appended
fn append_c_buffer_to(target: &mut String) -> bool {
    unsafe {
        let ptr = precc_get_buffer();
        let len = precc_get_buffer_len();
        if ptr.is_null() || len == 0 {
            return false;
        }
        // Track buffer copy stats for profiling
        if PROFILE_ENABLED.load(Ordering::Relaxed) {
            BUFFER_BYTES_COPIED.fetch_add(len as u64, Ordering::Relaxed);
            BUFFER_COPY_COUNT.fetch_add(1, Ordering::Relaxed);
        }
        let slice = std::slice::from_raw_parts(ptr as *const u8, len);
        // Fast path: if valid UTF-8, append directly without allocation
        if let Ok(s) = std::str::from_utf8(slice) {
            target.push_str(s);
        } else {
            // Fallback: handle invalid UTF-8 (should rarely happen for C source)
            target.push_str(&String::from_utf8_lossy(slice));
        }
        true
    }
}

/// Clear the C-side buffer
fn clear_c_buffer() {
    unsafe { precc_clear_buffer(); }
}

/// Check if C buffer ends with a specific character
fn c_buffer_ends_with(c: u8) -> bool {
    unsafe {
        let len = precc_get_buffer_len();
        if len == 0 {
            return false;
        }
        let ptr = precc_get_buffer();
        if ptr.is_null() {
            return false;
        }
        *ptr.add(len - 1) as u8 == c
    }
}

/// Check if C buffer is empty
fn c_buffer_is_empty() -> bool {
    unsafe { precc_get_buffer_len() == 0 }
}

// Use entry types from ctags_rs module
pub use ctags_rs::{ExtensionFields, FposT, TagEntryInfo};

// Version that takes &str directly - avoids redundant C string conversion
fn input_reset_skip_brace_str(kind_str: &str, name_str: &str, file_str: &str, scope_kind_str: &str, scope_name_str: &str) {
    // If this is a nested struct/union (has scope_kind = struct/union), don't overwrite
    // the parent's postponed entry - the nested struct will be included in the parent's code
    if scope_kind_str == "struct" || scope_kind_str == "union" {
        if DEBUG_TAGS_ENABLED.load(Ordering::Relaxed) {
            eprintln!("DEBUG input_reset_skip_brace: skipping nested {}:{} (scope={}:{})",
                     kind_str, name_str, scope_kind_str, scope_name_str);
        }
        return;
    }

    // Access postponed through with_tag_info (supports both global and thread-local)
    with_tag_info(|tag_info| {
        tag_info.postponed.kind = Some(kind_str.to_owned());
        tag_info.postponed.name = Some(name_str.to_owned());
        tag_info.postponed.file = Some(file_str.to_owned());
        tag_info.postponed.scope_kind = Some(scope_kind_str.to_owned());
        tag_info.postponed.scope_name = Some(scope_name_str.to_owned());
    });
}

pub fn input_reset_skip_brace(kind: *const c_char, name: *const c_char, file: *const c_char, scope_kind: *const c_char, scope_name: *const c_char) {
    let kind_str = if kind.is_null() { "" } else { unsafe { CStr::from_ptr(kind).to_str().unwrap_or("") } };
    let name_str = if name.is_null() { "" } else { unsafe { CStr::from_ptr(name).to_str().unwrap_or("") } };
    let file_str = if file.is_null() { "" } else { unsafe { CStr::from_ptr(file).to_str().unwrap_or("") } };
    let scope_kind_str = if scope_kind.is_null() { "" } else { unsafe { CStr::from_ptr(scope_kind).to_str().unwrap_or("") } };
    let scope_name_str = if scope_name.is_null() { "" } else { unsafe { CStr::from_ptr(scope_name).to_str().unwrap_or("") } };
    input_reset_skip_brace_str(kind_str, name_str, file_str, scope_kind_str, scope_name_str);
}

pub fn flush_input_buffer() {
    with_tag_info(|tag_info| {
        // Append directly from C-side buffer (zero-allocation, eliminates 2M FFI calls)
        append_c_buffer_to(&mut tag_info.lines);
    });
    clear_c_buffer();
}

#[no_mangle]
pub extern "C" fn output_an_entry(inside_brackets: i32, nest_level: i32) -> i32 {
    if DEBUG_TAGS_ENABLED.load(Ordering::Relaxed) {
        eprintln!("DEBUG output_an_entry: nest_level={}, c_buffer_is_empty={}", nest_level, c_buffer_is_empty());
    }
    if nest_level == 0 && !c_buffer_is_empty() {
        flush_input_buffer();

        // Extract values and clear while holding lock, then process after releasing
        let entry_data: Option<(String, String, String, String, String)> = with_tag_info(|tag_info| {
            if let (Some(kind_str), Some(name_str), Some(file_str)) =
                (&tag_info.postponed.kind, &tag_info.postponed.name, &tag_info.postponed.file) {
                let result = Some((
                    kind_str.clone(),
                    name_str.clone(),
                    file_str.clone(),
                    tag_info.postponed.scope_kind.as_deref().unwrap_or("").to_owned(),
                    tag_info.postponed.scope_name.as_deref().unwrap_or("").to_owned(),
                ));
                tag_info.postponed.clear();
                result
            } else {
                if DEBUG_TAGS_ENABLED.load(Ordering::Relaxed) {
                    eprintln!("DEBUG output_an_entry: postponed is empty! kind={:?} name={:?} file={:?}",
                        tag_info.postponed.kind, tag_info.postponed.name, tag_info.postponed.file);
                }
                None
            }
        });
        if let Some((kind, name, file, scope_kind, scope_name)) = entry_data {
            if DEBUG_TAGS_ENABLED.load(Ordering::Relaxed) {
                eprintln!("DEBUG output_an_entry: calling process_entry with kind={} name={}", kind, name);
            }
            let pu_type = PuType::from_str(&kind);
            let scope_type = if scope_kind.is_empty() { PuType::Unknown } else { PuType::from_str(&scope_kind) };
            process_entry(pu_type, &name, &file, scope_type, &scope_name);
        }
        return 0;
    }
    return inside_brackets;
}

#[no_mangle]
pub extern "C" fn output_an_entry_without_flush(nest_level: i32) {
    if nest_level == 0 && !c_buffer_is_empty() {
        // Extract values while holding lock, then process after releasing
        let entry_data: Option<(String, String, String, String, String)> = with_tag_info(|tag_info| {
            if tag_info.postponed.name.is_some() {
                let kind = tag_info.postponed.kind.as_deref().unwrap_or("");
                if kind.starts_with("enum") && kind == "struct" {
                    if let (Some(kind_str), Some(name_str), Some(file_str)) =
                        (&tag_info.postponed.kind, &tag_info.postponed.name, &tag_info.postponed.file) {
                        Some((
                            kind_str.clone(),
                            name_str.clone(),
                            file_str.clone(),
                            tag_info.postponed.scope_kind.as_deref().unwrap_or("").to_owned(),
                            tag_info.postponed.scope_name.as_deref().unwrap_or("").to_owned(),
                        ))
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            }
        });
        if let Some((kind, name, file, scope_kind, scope_name)) = entry_data {
            let pu_type = PuType::from_str(&kind);
            let scope_type = if scope_kind.is_empty() { PuType::Unknown } else { PuType::from_str(&scope_kind) };
            process_entry(pu_type, &name, &file, scope_type, &scope_name);
        }
    }
}

// Return type for input_reset_str: (need_skip_brace, scope_kind, scope_name, file)
// Returns whether caller needs to call input_reset_skip_brace_str
type PostponedCheckResult = Option<(String, String, String)>;

// Version that takes &str directly - avoids redundant C string conversion
// Returns info needed for postponed check to avoid re-locking
fn input_reset_str(kind_str: &str, name_str: &str, file_str: &str, scope_kind_str: &str, scope_name_str: &str) -> PostponedCheckResult {
    // All operations in single consolidated lock - avoids double lock/unlock cycle
    // Use with_tag_info for both global and thread-local modes
    with_tag_info(|tag_info| {
        input_reset_str_locked(tag_info, kind_str, name_str, file_str, scope_kind_str, scope_name_str)
    })
}

// Inner function that operates on a mutable TagInfo reference
fn input_reset_str_locked(tag_info: &mut TagInfo, kind_str: &str, name_str: &str, file_str: &str, scope_kind_str: &str, scope_name_str: &str) -> PostponedCheckResult {

    // Check if there's a postponed struct/union tag
    let postponed_struct_name: Option<(String, String)>;
    let prev_entry_data: Option<(String, String, String, String, String)>;

    if tag_info.postponed.is_struct_or_union() {
        if let (Some(ref pn), Some(ref pf)) = (&tag_info.postponed.name, &tag_info.postponed.file) {
            let pk = tag_info.postponed.kind.as_ref().unwrap();
            if DEBUG_TAGS_ENABLED.load(Ordering::Relaxed) {
                eprintln!("DEBUG input_reset: found postponed {}:{}, current tag is {}:{}", pk, pn, kind_str, name_str);
            }
            // If the next tag is a variable or typedef, the struct is defined inline
            if kind_str == "variable" || kind_str == "typedef" {
                postponed_struct_name = Some((pn.clone(), pf.clone()));
                prev_entry_data = None;
                tag_info.postponed.clear();
            } else if kind_str == "prototype" {
                // Bug57 fix: ctags sometimes emits a "prototype" tag for what is actually
                // a struct member declaration like `char_u *(cp_text[CPT_COUNT]);`
                // When we have a postponed struct and see a prototype tag, check if the
                // prototype name matches a typedef - this indicates it's likely a false
                // positive struct member, not a real prototype.
                //
                // In this case, do NOT finalize the struct yet - skip this tag and
                // continue buffering the struct body.
                //
                // Note: We can't use tag_info.tags here because it's not populated during
                // ctags processing - it gets populated later in post-processing. Instead,
                // check pu_order_set for entries starting with "typedef:NAME:"
                let typedef_prefix = format!("typedef:{}:", name_str);
                let is_false_positive = tag_info.pu_order_set.iter()
                    .any(|entry| entry.starts_with(&typedef_prefix));

                if DEBUG_TAGS_ENABLED.load(Ordering::Relaxed) {
                    eprintln!("DEBUG Bug57: Checking prototype:{} - is_false_positive={}",
                        name_str, is_false_positive);
                }

                if is_false_positive {
                    if DEBUG_TAGS_ENABLED.load(Ordering::Relaxed) {
                        eprintln!("DEBUG Bug57: Skipping false-positive prototype:{} inside postponed struct {}",
                            name_str, pn);
                    }
                    // Don't clear postponed - continue buffering the struct
                    postponed_struct_name = None;
                    prev_entry_data = None;
                } else {
                    // Real prototype - finalize the struct
                    prev_entry_data = Some((
                        pk.clone(),
                        pn.clone(),
                        pf.clone(),
                        tag_info.postponed.scope_kind.as_deref().unwrap_or("").to_owned(),
                        tag_info.postponed.scope_name.as_deref().unwrap_or("").to_owned(),
                    ));
                    postponed_struct_name = None;
                    tag_info.postponed.clear();
                }
            } else if scope_kind_str == "struct" || scope_kind_str == "union" {
                // This is a struct/union member - DON'T finalize the parent struct yet!
                // The struct body hasn't ended; the closing }; comes after all members.
                // Let output_an_entry at nest_level=0 handle finalization.
                if DEBUG_TAGS_ENABLED.load(Ordering::Relaxed) {
                    eprintln!("DEBUG input_reset: deferring struct finalization for member {}:{}", kind_str, name_str);
                }
                postponed_struct_name = None;
                prev_entry_data = None;
                // DON'T clear postponed - keep it for output_an_entry
            } else {
                // Non-variable/typedef/member tag follows - extract data for process_entry call
                prev_entry_data = Some((
                    pk.clone(),
                    pn.clone(),
                    pf.clone(),
                    tag_info.postponed.scope_kind.as_deref().unwrap_or("").to_owned(),
                    tag_info.postponed.scope_name.as_deref().unwrap_or("").to_owned(),
                ));
                postponed_struct_name = None;
                tag_info.postponed.clear();
            }
        } else {
            postponed_struct_name = None;
            prev_entry_data = None;
        }
    } else {
        postponed_struct_name = None;
        prev_entry_data = None;
    };

    // Append directly from C-side buffer (zero-allocation)
    append_c_buffer_to(&mut tag_info.lines);
    clear_c_buffer();

    // Bug35: Handle multi-name typedef merging
    // When ctags processes "typedef T *A, *B;", it emits:
    // - tag "A" with code "typedef T *A" (no semicolon)
    // - tag "B" with code ", *B;" (starts with comma)
    // We need to merge the second into the first
    let skip_current_entry = if kind_str == "typedef" {
        // Check if current typedef content starts with comma (continuation of previous)
        let trimmed = tag_info.lines.trim_start();
        // Skip line markers to find actual content start
        let content_start = if trimmed.starts_with('#') {
            // Find end of line marker and skip to actual content
            if let Some(newline_pos) = trimmed.find('\n') {
                trimmed[newline_pos + 1..].trim_start()
            } else {
                trimmed
            }
        } else {
            trimmed
        };

        // Check for continuation of multi-name typedef
        // ctags may emit content like "*XFontSet;" (pointer only) or ", *XFontSet;" (with comma)
        // for the second part of "typedef struct _XOC *XOC, *XFontSet;"
        let is_pointer_only = content_start.starts_with('*') && !content_start.contains("typedef");
        let is_comma_continuation = content_start.starts_with(',');

        if is_comma_continuation || (is_pointer_only && tag_info.incomplete_typedef.is_some()) {
            // This is a continuation of a multi-name typedef
            // Append to the primary typedef if we have one tracked
            if let Some((ref primary_key, ref primary_file)) = tag_info.incomplete_typedef.clone() {
                // File comparison: ctags may report the original .c file, so compare base names
                let current_file_base = file_str.rsplit('/').next().unwrap_or(file_str);
                let primary_file_base = primary_file.rsplit('/').next().unwrap_or(&primary_file);
                // Strip extension for comparison (bug35.c vs bug35.i should match)
                let current_stem = current_file_base.split('.').next().unwrap_or(current_file_base);
                let primary_stem = primary_file_base.split('.').next().unwrap_or(primary_file_base);

                if current_stem == primary_stem || file_str == primary_file {
                    // Take lines out to avoid borrow conflict
                    let continuation_lines = std::mem::take(&mut tag_info.lines);

                    // Append current content to the primary typedef's pu entry
                    // Need to add comma before pointer-only continuations
                    // Bug35: After merging, we need to clone the code and insert a new pu entry
                    // for the continuation typedef name. Use a block to limit mutable borrow scope.
                    let (merged_code, typedef_complete) = {
                        if let Some(primary_code) = tag_info.pu.get_mut(primary_key.as_str()) {
                            // Check if primary code already ends with a comma (ctags may include it)
                            let primary_trimmed = primary_code.trim_end();
                            let already_has_comma = primary_trimmed.ends_with(',');

                            if is_pointer_only && !is_comma_continuation && !already_has_comma {
                                // Add the missing comma before the pointer declaration
                                primary_code.push_str(", ");
                            } else if is_pointer_only && !is_comma_continuation && already_has_comma {
                                // Primary already has comma, just add a space
                                primary_code.push_str(" ");
                            }
                            primary_code.push_str(&continuation_lines);

                            // Clone the merged code and check completion status
                            let code = primary_code.clone();
                            let complete = code.trim_end().ends_with(';');
                            (Some(code), complete)
                        } else {
                            if DEBUG_TAGS_ENABLED.load(Ordering::Relaxed) {
                                eprintln!("DEBUG Bug35: key '{}' not found in pu map", primary_key);
                            }
                            (None, false)
                        }
                    };

                    if let Some(code) = merged_code {
                        // Bug35 fix: Add the continuation typedef name to tags map AND pu map
                        // so that it can be resolved when other code references it.
                        // The continuation name (e.g., XFontSet) should resolve to the
                        // merged typedef code. We need both:
                        // 1. tags entry: "XFontSet" -> "typedef:bug35.i"
                        // 2. pu entry: "typedef:XFontSet:bug35.i" -> merged code
                        // This allows "XFontSet fs;" to find the complete typedef definition.
                        tag_info.tags.entry(name_str.to_owned())
                            .or_default()
                            .push(format!("typedef:{}", file_str));

                        // Also add a pu entry for the continuation typedef
                        // pointing to the same merged code as the primary typedef
                        let continuation_pu_key = format!("typedef:{}:{}", name_str, file_str);
                        tag_info.pu.insert(continuation_pu_key, code);

                        // Check if this completes the typedef (ends with semicolon)
                        if typedef_complete {
                            tag_info.incomplete_typedef = None;
                        }
                    }
                    // Clear to_dep so we don't process dependencies for this skipped entry
                    tag_info.to_dep.clear();
                    true  // Skip processing this entry
                } else {
                    if DEBUG_TAGS_ENABLED.load(Ordering::Relaxed) {
                        eprintln!("DEBUG Bug35: file mismatch: current='{}' primary='{}'", file_str, primary_file);
                    }
                    false
                }
            } else {
                false
            }
        } else {
            // New typedef - check if it's incomplete (doesn't end with semicolon)
            let trimmed_end = tag_info.lines.trim_end();
            if !trimmed_end.ends_with(';') && !trimmed_end.is_empty() {
                // This typedef doesn't end with semicolon - it's the start of a multi-name typedef
                let unit_key = make_unit_key("typedef", name_str, file_str);
                tag_info.incomplete_typedef = Some((unit_key, file_str.to_owned()));
            } else {
                // Complete typedef - clear any incomplete state
                tag_info.incomplete_typedef = None;
            }
            false
        }
    } else {
        // Non-typedef tag - clear incomplete typedef state
        tag_info.incomplete_typedef = None;
        false
    };

    // If we're skipping this entry (multi-name typedef continuation), return early
    if skip_current_entry {
        // Still need to check postponed state
        if let Some(ref kind) = tag_info.postponed.kind {
            if kind == "enumerator" {
                tag_info.postponed.kind = None;
                tag_info.postponed.name = None;
                return None;
            } else if tag_info.postponed.name.is_none() {
                return Some((scope_kind_str.to_owned(), scope_name_str.to_owned(), file_str.to_owned()));
            }
        }
        return None;
    }

    // If we have a postponed struct, add alias now while we hold the lock
    if let Some((ref struct_name, _)) = postponed_struct_name {
        let alias = format!("{}:{}:{}", kind_str, name_str, file_str);
        if DEBUG_TAGS_ENABLED.load(Ordering::Relaxed) {
            eprintln!("DEBUG input_reset: adding alias {} -> {}", struct_name, alias);
        }
        tag_info.tags.entry(struct_name.clone()).or_default().push(alias);
    }

    // Process previous entry if needed (with lock held - no re-locking)
    if let Some((ref pk, ref pn, ref pf, ref p_scope_kind, ref p_scope_name)) = prev_entry_data {
        let prev_pu_type = PuType::from_str(pk);
        let prev_scope_type = if p_scope_kind.is_empty() { PuType::Unknown } else { PuType::from_str(p_scope_kind) };
        // Skip struct/union members only - prototypes are now processed for dependency tracking
        if !matches!(prev_scope_type, PuType::Struct | PuType::Union) {
            process_entry_locked(tag_info, prev_pu_type, pn, pf, prev_scope_type, p_scope_name);
        }
    }

    // Process the current entry (with lock held - no re-locking)
    let curr_pu_type = PuType::from_str(kind_str);
    let curr_scope_type = if scope_kind_str.is_empty() { PuType::Unknown } else { PuType::from_str(scope_kind_str) };
    // Skip struct/union members only - prototypes are now processed for dependency tracking
    if !matches!(curr_scope_type, PuType::Struct | PuType::Union) {
        process_entry_locked(tag_info, curr_pu_type, name_str, file_str, curr_scope_type, scope_name_str);
    }

    // Check postponed state and return info for caller (with lock still held)
    // This avoids the caller needing to re-lock to check postponed
    if let Some(ref kind) = tag_info.postponed.kind {
        if kind == "enumerator" {
            tag_info.postponed.kind = None;
            tag_info.postponed.name = None;
            None
        } else if tag_info.postponed.name.is_none() {
            // Need to call input_reset_skip_brace_str after releasing lock
            Some((scope_kind_str.to_owned(), scope_name_str.to_owned(), file_str.to_owned()))
        } else {
            None
        }
    } else {
        None
    }
}

pub fn input_reset(kind: *const c_char, name: *const c_char, file: *const c_char, scope_kind: *const c_char, scope_name: *const c_char) {
    // Convert C strings once at the start
    let kind_str = if kind.is_null() { "" } else { unsafe { CStr::from_ptr(kind).to_str().unwrap_or("") } };
    let name_str = if name.is_null() { "" } else { unsafe { CStr::from_ptr(name).to_str().unwrap_or("") } };
    let file_str = if file.is_null() { "" } else { unsafe { CStr::from_ptr(file).to_str().unwrap_or("") } };
    let scope_kind_str = if scope_kind.is_null() { "" } else { unsafe { CStr::from_ptr(scope_kind).to_str().unwrap_or("") } };
    let scope_name_str = if scope_name.is_null() { "" } else { unsafe { CStr::from_ptr(scope_name).to_str().unwrap_or("") } };
    input_reset_str(kind_str, name_str, file_str, scope_kind_str, scope_name_str);
}

// Note: precc_putchar is kept for non-PRECC_FAST_PATH builds but not used in fast path
// In fast path, characters are written directly to C-side buffer
#[no_mangle]
pub fn precc_putchar(c: c_char) {
    if PROFILE_ENABLED.load(Ordering::Relaxed) {
        PUTCHAR_COUNT.fetch_add(1, Ordering::Relaxed);
    }
    INPUT_BUFFER.with(|buffer| buffer.borrow_mut().push(c as u8));
}

#[no_mangle]
pub extern "C" fn precc_debugEntry(tag: *const TagEntryInfo, kind_id: u8, scope_kind_id: u8) {
    let start = if PROFILE_ENABLED.load(Ordering::Relaxed) { Some(Instant::now()) } else { None };
    DEBUG_ENTRY_COUNT.fetch_add(1, Ordering::Relaxed);

    let tag = unsafe { &*tag };

    // Convert numeric kind_id to PuType enum - O(1) direct mapping, no string parsing!
    let pu_type = PuType::from_id(kind_id);
    let scope_type = PuType::from_id(scope_kind_id);

    // NOTE: Prototypes are now processed to enable dependency tracking for extern functions
    // They are stored in pu/tags but NOT added to pu_order (so they don't get separate split files)

    // Check C-side buffer for trailing brace (no FFI overhead - just pointer access)
    let ends_with_brace = c_buffer_ends_with(b'{');

    // Get string representations from enum - these are static &str, no allocation
    let kind_str = pu_type.as_str();
    let scope_kind_str = scope_type.as_str();

    // Convert remaining C strings only for non-prototype tags (name and file still needed)
    let name_str = if tag.name.is_null() { "" } else { unsafe { CStr::from_ptr(tag.name).to_str().unwrap_or("") } };
    let file_str = if tag.source_file_name.is_null() { "" } else { unsafe { CStr::from_ptr(tag.source_file_name).to_str().unwrap_or("") } };
    let scope_name_str = if tag.extension_fields.scope[1].is_null() { "" } else { unsafe { CStr::from_ptr(tag.extension_fields.scope[1]).to_str().unwrap_or("") } };

    // Store line number for body extraction (only for functions, variables, structs, typedefs)
    if matches!(pu_type, PuType::Function | PuType::Variable | PuType::Struct | PuType::Union | PuType::Typedef | PuType::Enum) {
        let line_no = tag.line_number;
        let u = make_unit_key(kind_str, name_str, file_str);
        with_tag_info(|tag_info| {
            tag_info.line_numbers.insert(u, line_no);
        });
    }

    if DEBUG_TAGS_ENABLED.load(Ordering::Relaxed) {
        eprintln!("DEBUG precc_debugEntry: kind='{}' name='{}' ends_with_brace={} line={}", kind_str, name_str, ends_with_brace, tag.line_number);
    }
    if ends_with_brace {
        // Use optimized _str version - no redundant C string conversion
        input_reset_skip_brace_str(kind_str, name_str, file_str, scope_kind_str, scope_name_str);
    } else {
        // Use optimized _str version - no redundant C string conversion
        // Returns info needed for postponed check to avoid re-locking
        if let Some((scope_kind, scope_name, file)) = input_reset_str(kind_str, name_str, file_str, scope_kind_str, scope_name_str) {
            // Use scope_kind/scope_name as kind/name for this call
            input_reset_skip_brace_str(&scope_kind, &scope_name, &file, &scope_kind, &scope_name);
        }
    }

    if let Some(s) = start {
        DEBUG_ENTRY_TIME_NS.fetch_add(s.elapsed().as_nanos() as u64, Ordering::Relaxed);
    }
}

#[no_mangle]
pub extern "C" fn depends_on(token: *const c_char) {
        let start = if PROFILE_ENABLED.load(Ordering::Relaxed) { Some(Instant::now()) } else { None };
        DEPENDS_ON_COUNT.fetch_add(1, Ordering::Relaxed);

        let token_str = unsafe { CStr::from_ptr(token).to_str().unwrap() };
        with_tag_info(|tag_info| tag_info.to_dep.push(token_str.to_string()));

        if let Some(s) = start {
            DEPENDS_ON_TIME_NS.fetch_add(s.elapsed().as_nanos() as u64, Ordering::Relaxed);
        }
}

fn process_entry(pu_type: PuType, name: &str, file_str: &str, scope_type: PuType, scope_name: &str) {
    // Early return for struct/union members - no allocations needed
    if matches!(scope_type, PuType::Struct | PuType::Union) {
        if DEBUG_TAGS_ENABLED.load(Ordering::Relaxed) {
            eprintln!("DEBUG process_entry: skipping {}:{} due to scope={:?}:{}",
                pu_type.as_str(), name, scope_type, scope_name);
        }
        return;
    }
    if DEBUG_TAGS_ENABLED.load(Ordering::Relaxed) {
        eprintln!("DEBUG process_entry: processing {}:{} file={}", pu_type.as_str(), name, file_str);
    }

    with_tag_info(|tag_info| {
        // Prototypes are handled specially - store in pu and tags for dependency resolution,
        // but NOT added to pu_order (so they don't get their own split files)
        if pu_type == PuType::Prototype {
            let u = make_unit_key(pu_type.as_str(), name, file_str);

            // Store prototype code in pu map
            let lines = std::mem::take(&mut tag_info.lines);
            tag_info.pu.insert(u.clone(), lines);

            // Add to tags map for dependency resolution (name -> type:file)
            tag_info.tags.entry(name.to_string())
                .or_default()
                .push(format!("{}:{}", pu_type.as_str(), file_str));

            // Clear to_dep since prototypes don't propagate dependencies
            tag_info.to_dep.clear();
            return;
        }

        process_entry_locked(tag_info, pu_type, name, file_str, scope_type, scope_name);
    });
}

// Inner function that takes already-locked TagInfo to avoid double-locking
#[inline(always)]
fn process_entry_locked(tag_info: &mut TagInfo, pu_type: PuType, name: &str, file_str: &str, scope_type: PuType, scope_name: &str) {
    let start = if PROFILE_ENABLED.load(Ordering::Relaxed) { Some(Instant::now()) } else { None };
    PROCESS_ENTRY_COUNT.fetch_add(1, Ordering::Relaxed);

    // Prototypes are handled specially - store in pu and tags for dependency resolution,
    // but NOT added to pu_order (so they don't get their own split files)
    if pu_type == PuType::Prototype {
        let u = make_unit_key(pu_type.as_str(), name, file_str);

        // Store prototype code in pu map
        let lines = std::mem::take(&mut tag_info.lines);
        tag_info.pu.insert(u.clone(), lines);

        // Add to tags map for dependency resolution (name -> type:file)
        tag_info.tags.entry(name.to_string())
            .or_default()
            .push(format!("{}:{}", pu_type.as_str(), file_str));

        // Clear to_dep since prototypes don't propagate dependencies
        tag_info.to_dep.clear();

        if let Some(s) = start {
            PROCESS_ENTRY_TIME_NS.fetch_add(s.elapsed().as_nanos() as u64, Ordering::Relaxed);
        }
        return;
    }

    {
        // Use the canonical enum string for consistent keys
        let u = make_unit_key(pu_type.as_str(), name, file_str);

        if DEBUG_TAGS_ENABLED.load(Ordering::Relaxed) {
            eprintln!("DEBUG process_entry_locked: u={} lines_len={}", u, tag_info.lines.len());
        }

        // Check first with borrow, only clone if new
        if !tag_info.pu_order_set.contains(&u) {
            tag_info.pu_order_set.insert(u.clone());
            tag_info.pu_order.push(u.clone());
            if DEBUG_TAGS_ENABLED.load(Ordering::Relaxed) {
                eprintln!("DEBUG process_entry_locked: ADDED to pu_order: {} (now has {} entries)", u, tag_info.pu_order.len());
            }
        }
        // Use enum comparison instead of string comparison
        if matches!(pu_type, PuType::Enum | PuType::Typedef)
            && tag_info.lines.contains("enum")
            && tag_info.lines.contains("{")
        {
            let mut to_dep = std::mem::take(&mut tag_info.to_dep);
            let mut dep = std::mem::take(&mut tag_info.dep);
            let mut tags = std::mem::take(&mut tag_info.tags);

            // Collect current enumerator names before potentially remapping
            let current_enumerators: Vec<String> = to_dep.iter()
                .filter(|k| k.starts_with("enumerator:"))
                .filter_map(|k| {
                    let parts: Vec<&str> = k.split(':').collect();
                    if parts.len() >= 2 { Some(parts[1].to_string()) } else { None }
                })
                .collect();

            // Detect anonymous enum key collision: if pu already has a body under u
            // and the current to_dep enumerators don't appear in the existing body,
            // this is a second anonymous enum with the same ctags __anonN name.
            // Generate a unique key to avoid overwriting the first enum's body.
            let effective_u = if name.starts_with("__anon") && tag_info.pu.get(&u).map_or(false, |b| !b.is_empty()) {
                // Check if the existing body contains any of the current enumerators
                // If yes, this is the same enum (body already stored correctly).
                // If no, this is a collision — a different anonymous enum with the same __anonN name.
                let existing_body = tag_info.pu.get(&u).map(|s| s.as_str()).unwrap_or("");
                let is_same_enum = current_enumerators.iter()
                    .any(|e| existing_body.contains(e.as_str()));
                if is_same_enum {
                    u.clone()
                } else {
                    // Collision: find a unique key by appending a suffix
                    // u = "enum:__anon63:file" -> try "enum:__anon63_2:file", etc.
                    let mut suffix = 2usize;
                    let unique_u = loop {
                        let candidate = format!("enum:{name}_{suffix}:{file_str}");
                        if !tag_info.pu.contains_key(&candidate) {
                            break candidate;
                        }
                        suffix += 1;
                    };
                    // Also add to pu_order and pu_order_set for the new key
                    if !tag_info.pu_order_set.contains(&unique_u) {
                        tag_info.pu_order_set.insert(unique_u.clone());
                        tag_info.pu_order.push(unique_u.clone());
                    }
                    unique_u
                }
            } else {
                u.clone()
            };

            // Record enumerator->enum mapping for all enumerators in this enum
            // This is needed to resolve dependencies on anonymous enums
            // Must be done before borrowing pu immutably
            for enumerator_name in &current_enumerators {
                tag_info.enumerator_to_enum.insert(enumerator_name.clone(), effective_u.clone());
            }

            // Now borrow pu immutably after all mutations are done
            let pu_ref = &tag_info.pu;

            process_enum_or_typedef(
                &mut to_dep,
                &mut dep,
                &mut tags,
                pu_ref,
                pu_type,
                &name,
                &file_str,
                &effective_u,
            );

            to_dep.clear();
            tag_info.to_dep = to_dep;
            tag_info.dep = dep;
            tag_info.tags = tags;

            let lines = std::mem::take(&mut tag_info.lines);
            tag_info.pu.insert(effective_u.clone(), lines);
        } else if pu_type == PuType::Enumerator {
            tag_info.to_dep.push(u.clone());

            // Bug60 fix: Add enumerator to tags map for dependency resolution
            // This enables functions that depend on enum constants to find them
            tag_info.tags.entry(name.to_owned())
                .or_default()
                .push(format!("{}:{}", pu_type.as_str(), file_str));

            // Handle anonymous enums: if this enumerator belongs to an anonymous enum
            // (scope_type == Enum and scope_name starts with "__anon"), we need to
            // map the enumerator to its parent enum for dependency resolution
            if scope_type == PuType::Enum && scope_name.starts_with("__anon") {
                // Create a synthetic enum unit key for this anonymous enum
                let anon_capacity = 5 + scope_name.len() + 1 + file_str.len();
                let mut anon_enum_unit = String::with_capacity(anon_capacity);
                anon_enum_unit.push_str("enum:");
                anon_enum_unit.push_str(scope_name);
                anon_enum_unit.push(':');
                anon_enum_unit.push_str(file_str);

                // Map this enumerator to the anonymous enum unit
                tag_info.enumerator_to_enum.insert(name.to_owned(), anon_enum_unit.clone());

                // Track the anonymous enum unit if not seen before
                if !tag_info.anon_enum_units.contains_key(scope_name) {
                    tag_info.anon_enum_units.insert(scope_name.to_owned(), anon_enum_unit);
                }
            } else {
                // For named enums, also add enumerator_to_enum mapping
                // The parent enum unit key is "enum:<scope_name>:<file>"
                let named_enum_unit = format!("enum:{}:{}", scope_name, file_str);
                tag_info.enumerator_to_enum.insert(name.to_owned(), named_enum_unit);
            }

            // Use original u instead of clone - saves one allocation per enumerator
            tag_info.pu.insert(u, String::new());
        } else if pu_type == PuType::ExternVar {
            let mut lines = std::mem::take(&mut tag_info.lines);
            let mut headlines = std::mem::take(&mut tag_info.headlines);
            let mut to_dep = std::mem::take(&mut tag_info.to_dep);
            let mut dep = std::mem::take(&mut tag_info.dep);
            
            process_externvar(
                &mut lines,
                &mut headlines,
                &mut to_dep,
                &mut dep,
                &name,
                &u,
            );
            
            tag_info.lines = lines;
            tag_info.headlines = headlines;
            tag_info.to_dep = to_dep;
            tag_info.dep = dep;

            let lines = std::mem::take(&mut tag_info.lines);
            // Use original u instead of clone - saves one allocation per externvar
            tag_info.pu.insert(u, lines);
        } else {
            // Avoid cloning to_dep - iterate directly with indices
            let mut dep = std::mem::take(&mut tag_info.dep);
            let to_dep_len = tag_info.to_dep.len();

            // Use entry API to avoid double lookups, reserve capacity upfront
            let dep_entry = dep.entry(u.clone()).or_insert_with(|| Vec::with_capacity(to_dep_len));
            for to_dep_l in tag_info.to_dep.iter() {
                dep_entry.push(to_dep_l.clone());
            }

            // Handle forward typedef declarations for structs/unions
            // Pattern: "typedef struct X Y;" or "typedef struct X X;" without body
            // These need a dependency on the struct definition so the full struct is included
            if pu_type == PuType::Typedef {
                let lines_ref = &tag_info.lines;
                let trimmed = lines_ref.trim();

                // Check if this is a forward declaration (no brace = no body)
                if !trimmed.contains('{') {
                    // Extract struct/union name from patterns like:
                    // "typedef struct Hash Hash;" -> struct name is "Hash"
                    // "typedef struct _foo Foo;" -> struct name is "_foo"
                    // "typedef union Bar Bar;" -> union name is "Bar"
                    let patterns = [
                        ("typedef struct ", "struct"),
                        ("typedef union ", "union"),
                    ];

                    for (pattern, kind) in patterns.iter() {
                        if trimmed.contains(pattern) {
                            // Extract the struct/union name (first identifier after "struct " or "union ")
                            if let Some(start) = trimmed.find(pattern) {
                                let after_keyword = &trimmed[start + pattern.len()..];
                                // Get the first word (struct/union name)
                                let struct_name: String = after_keyword
                                    .chars()
                                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                                    .collect();

                                if !struct_name.is_empty() {
                                    // Add dependency on the struct/union definition
                                    // This ensures "struct:Hash:file" is included when "typedef:Hash:file" is needed
                                    // Use entry API - the entry for u was already created above
                                    dep.entry(u.clone()).or_default().push(struct_name.clone());
                                    if DEBUG_TAGS_ENABLED.load(Ordering::Relaxed) {
                                        if struct_name != name {
                                            eprintln!("DEBUG: typedef {} forward declares {}, adding dependency on {}", name, struct_name, kind);
                                        } else {
                                            eprintln!("DEBUG: typedef {} is self-referential forward decl, adding dependency on {}", name, kind);
                                        }
                                    }
                                }
                            }
                            break;
                        }
                    }
                }
            }

            // Handle comma-separated variable declarations
            // When a variable doesn't have a type prefix, it's a continuation of a previous declaration
            // e.g., "static long x, y;" -> x has type, y is just " y;"
            // Strategy: Merge continuation code into previous variable and create an alias
            if pu_type == PuType::Variable {
                let lines_ref = &tag_info.lines;
                let trimmed = lines_ref.trim_start();

                // Check if this variable has a proper type declaration
                let has_type = trimmed.starts_with("static ")
                    || trimmed.starts_with("extern ")
                    || trimmed.starts_with("const ")
                    || trimmed.starts_with("volatile ")
                    || trimmed.starts_with("register ")
                    || trimmed.starts_with("_Thread_local ")
                    || trimmed.starts_with("__thread ")
                    || trimmed.starts_with("unsigned ")
                    || trimmed.starts_with("signed ")
                    || trimmed.starts_with("long ")
                    || trimmed.starts_with("short ")
                    || trimmed.starts_with("int ")
                    || trimmed.starts_with("char ")
                    || trimmed.starts_with("float ")
                    || trimmed.starts_with("double ")
                    || trimmed.starts_with("void ")
                    || trimmed.starts_with("_Bool ")
                    || trimmed.starts_with("struct ")
                    || trimmed.starts_with("union ")
                    || trimmed.starts_with("enum ")
                    || (trimmed.contains(" ") && !trimmed.starts_with(" "));  // Has type + name, not starting with space

                if has_type {
                    // This variable has a proper type - record it as the last typed variable
                    tag_info.last_typed_variable = Some((u.clone(), name.to_owned()));
                    if DEBUG_TAGS_ENABLED.load(Ordering::Relaxed) {
                        eprintln!("DEBUG process_entry: variable {} has type, setting as last_typed_variable", name);
                    }
                } else {
                    // This is a comma-continuation variable
                    // Merge its code into the previous variable's code and create an alias
                    if let Some((prev_var_key, prev_var_name)) = tag_info.last_typed_variable.clone() {
                        if DEBUG_TAGS_ENABLED.load(Ordering::Relaxed) {
                            eprintln!("DEBUG process_entry: variable {} is continuation of {}, merging", name, prev_var_name);
                        }
                        // Clone lines before borrowing pu mutably
                        let lines_to_append = tag_info.lines.clone();
                        // Append this variable's code to the previous variable's code
                        if let Some(prev_code) = tag_info.pu.get_mut(&prev_var_key) {
                            prev_code.push_str(&lines_to_append);
                        }
                        // Create an alias so references to this variable resolve to the previous variable
                        tag_info.tags.entry(name.to_owned()).or_default().push(prev_var_key.clone());
                        // Clear lines since we've merged them
                        tag_info.lines.clear();
                    }
                }
            }

            tag_info.dep = dep;
            tag_info.to_dep.clear();

            let lines = std::mem::take(&mut tag_info.lines);

            // Skip malformed typedefs that are just pointer declarators without a proper type
            // This happens with comma-separated typedefs like "} XrmValue, *XrmValuePtr;"
            // where ctags captures XrmValuePtr with content "*XrmValuePtr;" which is incomplete
            if pu_type == PuType::Typedef {
                let trimmed = lines.trim();
                // Skip if it starts with * (pointer only, no type) or is just a name followed by semicolon
                if trimmed.starts_with('*') || (trimmed.len() > 0 && !trimmed.contains(' ') && !trimmed.contains("typedef")) {
                    // Don't insert malformed typedef - it would corrupt the output
                    if DEBUG_TAGS_ENABLED.load(Ordering::Relaxed) {
                        eprintln!("DEBUG: Skipping malformed typedef '{}': content '{}'", name, trimmed);
                    }
                    return;
                }
            }

            // Use original u instead of clone - saves one allocation
            tag_info.pu.insert(u, lines);
        }
        // tag_info.pu_order.push(u.clone());
    }
    if let Some(s) = start {
        PROCESS_ENTRY_TIME_NS.fetch_add(s.elapsed().as_nanos() as u64, Ordering::Relaxed);
    }
}

pub fn clear_input_buffer() {
    // Clear both C-side buffer and Rust-side buffer (for compatibility)
    clear_c_buffer();
    INPUT_BUFFER.with(|buffer| buffer.borrow_mut().clear());
}

/// Extract FileAnalysis from a preprocessed file for cross-file dependency analysis
/// This processes the file with ctags and returns the analysis data without generating PU files
pub fn extract_file_analysis(filename: &str) -> io::Result<crossfile::FileAnalysis> {
    use crossfile::FileAnalysisBuilder;

    // Initialize cached env vars
    init_debug_tags();

    // Reset global state for fresh processing
    with_tag_info(|tag_info| *tag_info = TagInfo::default());
    clear_input_buffer();

    // Process file with ctags
    let dctags = DCTags::new()
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
    let _ = dctags.process_file_direct(filename);

    // Build FileAnalysis from TagInfo using with_tag_info
    with_tag_info(|tag_info| {
        let mut builder = FileAnalysisBuilder::new(filename);

        // Add all symbols
        for (key, code) in &tag_info.pu {
            builder.add_symbol(key, code);
        }

        // Add dependencies
        for (key, deps) in &tag_info.dep {
            // Extract function name from key (format: "function:name:file")
            if let Some(name) = key.splitn(3, ':').nth(1) {
                for dep_key in deps {
                    // Extract dependency name
                    if let Some(dep_name) = dep_key.splitn(3, ':').nth(1) {
                        builder.add_dependency(name, dep_name);
                    }
                }
            }
        }

        // Identify external references (symbols used but not defined)
        // A symbol is external if it appears in dependencies but not in pu
        for deps in tag_info.dep.values() {
            for dep_key in deps {
                if !tag_info.pu.contains_key(dep_key) {
                    if let Some(name) = dep_key.splitn(3, ':').nth(1) {
                        builder.add_external_ref(name);
                    }
                }
            }
        }

        Ok(builder.build())
    })
}

/// Analyze multiple files and build cross-file dependency graph
/// This is the main entry point for cross-file analysis
pub fn analyze_project_dependencies(filenames: &[String]) -> io::Result<crossfile::CrossFileDeps> {
    use std::time::Instant;

    let start = Instant::now();
    eprintln!("Analyzing {} files for cross-file dependencies...", filenames.len());

    let mut file_analyses = Vec::with_capacity(filenames.len());

    for (i, filename) in filenames.iter().enumerate() {
        eprint!("\r  [{}/{}] Analyzing: {}                    ",
            i + 1, filenames.len(),
            std::path::Path::new(filename).file_name().unwrap_or_default().to_string_lossy());

        match extract_file_analysis(filename) {
            Ok(analysis) => {
                file_analyses.push(analysis);
            }
            Err(e) => {
                eprintln!("\nWarning: Failed to analyze {}: {}", filename, e);
            }
        }
    }

    eprintln!("\r  Analyzed {} files in {:.2}s                              ",
        file_analyses.len(), start.elapsed().as_secs_f64());

    let deps = crossfile::CrossFileDeps::analyze_files(file_analyses);
    Ok(deps)
}
