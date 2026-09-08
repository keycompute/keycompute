pub(crate) const DEFAULT_PAGE_SIZE: i64 = 20;
pub(crate) const MAX_PAGE_SIZE: i64 = 100;

pub(crate) fn normalize_list_pagination(
    page: Option<i64>,
    page_size: Option<i64>,
    legacy_limit: Option<i64>,
    legacy_offset: Option<i64>,
) -> (i64, i64, i64) {
    let page_size = page_size
        .or(legacy_limit)
        .unwrap_or(DEFAULT_PAGE_SIZE)
        .clamp(1, MAX_PAGE_SIZE);
    match page {
        Some(page) => {
            let page = page.clamp(1, 1_000_000);
            (page, page_size, (page - 1) * page_size)
        }
        None => {
            let offset = legacy_offset.unwrap_or(0).max(0);
            let page = (offset / page_size).saturating_add(1).clamp(1, 1_000_000);
            (page, page_size, offset)
        }
    }
}

pub(crate) fn total_pages(total: i64, page_size: i64) -> i64 {
    let total = total.max(0);
    let page_size = page_size.max(1);
    total / page_size + i64::from(total % page_size != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_pagination_supports_modern_and_legacy_parameters() {
        assert_eq!(
            normalize_list_pagination(Some(3), Some(25), Some(10), Some(0)),
            (3, 25, 50)
        );
        assert_eq!(
            normalize_list_pagination(None, None, Some(50), Some(100)),
            (3, 50, 100)
        );
    }

    #[test]
    fn legacy_pagination_preserves_non_aligned_offsets() {
        assert_eq!(
            normalize_list_pagination(None, None, Some(20), Some(5)),
            (1, 20, 5)
        );
        assert_eq!(
            normalize_list_pagination(None, None, Some(20), Some(25)),
            (2, 20, 25)
        );
    }

    #[test]
    fn list_pagination_clamps_untrusted_values() {
        assert_eq!(
            normalize_list_pagination(Some(-10), Some(10_000), None, None),
            (1, MAX_PAGE_SIZE, 0)
        );
        assert_eq!(
            normalize_list_pagination(None, Some(0), None, Some(-10)),
            (1, 1, 0)
        );
        assert_eq!(
            normalize_list_pagination(None, Some(1), None, Some(i64::MAX)),
            (1_000_000, 1, i64::MAX)
        );
        assert_eq!(total_pages(0, DEFAULT_PAGE_SIZE), 0);
        assert_eq!(total_pages(41, DEFAULT_PAGE_SIZE), 3);
    }
}
