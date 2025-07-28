//! SQL Analysis Module
//! 
//! This module provides comprehensive SQL query analysis capabilities including:
//! - Advanced normalization using AST parsing
//! - Query complexity analysis
//! - Metadata extraction
//! - Fingerprinting for accurate query grouping

pub mod normalization;

pub use normalization::{
    QueryNormalizer, NormalizationConfig, NormalizationResult, 
    LiteralInfo, LiteralType, normalize_query_enhanced, calculate_query_fingerprint
};