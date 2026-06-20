//! SQL Analysis Module
//!
//! This module provides comprehensive SQL query analysis capabilities including:
//! - Advanced normalization using AST parsing
//! - Query complexity analysis and scoring
//! - Metadata extraction and classification
//! - Performance regression detection
//! - Fingerprinting for accurate query grouping

pub mod anti_patterns;
pub mod complexity;
pub mod metadata;
pub mod normalization;
pub mod regression;
pub mod statistics;

#[cfg(test)]
pub mod tests;

pub use anti_patterns::{AntiPattern, AntiPatternAnalyzer, AntiPatternKind, AntiPatternSeverity};

pub use normalization::{
    LiteralInfo, LiteralType, NormalizationResult, QueryNormalizer, calculate_query_fingerprint,
    normalize_query_enhanced,
};

pub use complexity::{
    AggregationInfo, ComplexityAnalyzer, ComplexityBreakdown, ComplexityClass,
    ComplexityComponents, ComplexityScore, ConditionInfo, FunctionInfo, JoinInfo, SubqueryInfo,
};

pub use metadata::{
    AccessPattern, ColumnReference, ExecutionPattern, FunctionReference, MetadataExtractor,
    PerformanceHint, QueryClassification, QueryMetadata, QueryOperation, TableReference,
};

pub use regression::{
    MetricRegression, PerformanceDataPoint, PerformanceMetric, RegressionAnalysis,
    RegressionDetector, RegressionSeverity, RegressionStatus,
};
