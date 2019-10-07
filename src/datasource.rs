use crate::pagelist::*;
use crate::platform::Platform;
use mysql as my;
use rayon::prelude::*;
use serde_json::value::Value;
use std::time;
use wikibase::mediawiki::api::Api;
use wikibase::mediawiki::title::Title;

pub type SQLtuple = (String, Vec<String>);

pub trait DataSource {
    fn can_run(&self, platform: &Platform) -> bool;
    fn run(&mut self, platform: &Platform) -> Result<PageList, String>;
    fn name(&self) -> String;
}

//________________________________________________________________________________________________________________________

#[derive(Debug, Clone, PartialEq)]
pub struct SourceLabels {}

impl DataSource for SourceLabels {
    fn name(&self) -> String {
        "labels".to_string()
    }

    fn can_run(&self, platform: &Platform) -> bool {
        platform.has_param("labels_yes") || platform.has_param("labels_any")
    }

    fn run(&mut self, platform: &Platform) -> Result<PageList, String> {
        let state = platform.state();
        let db_user_pass = match state.get_db_mutex().lock() {
            Ok(db) => db,
            Err(e) => return Err(format!("Bad mutex: {:?}", e)),
        };
        let sql = platform.get_label_sql();
        let mut conn = platform
            .state()
            .get_wiki_db_connection(&db_user_pass, &"wikidatawiki".to_string())?;
        let result = match conn.prep_exec(sql.0, sql.1) {
            Ok(r) => r,
            Err(e) => return Err(format!("{:?}", e)),
        };

        let entries_tmp = result
            .filter_map(|row_result| row_result.ok())
            .filter_map(|row| Platform::entry_from_entity(&my::from_row::<String>(row)))
            .collect();
        Ok(PageList::new_from_vec(
            &"wikidatawiki".to_string(),
            entries_tmp,
        ))
    }
}

impl SourceLabels {
    pub fn new() -> Self {
        Self {}
    }
}

//________________________________________________________________________________________________________________________

#[derive(Debug, Clone, PartialEq)]
pub struct SourceWikidata {}

impl DataSource for SourceWikidata {
    fn name(&self) -> String {
        "wikidata".to_string()
    }

    fn can_run(&self, platform: &Platform) -> bool {
        platform.has_param("wpiu_no_statements") && platform.has_param("wikidata_source_sites")
    }

    fn run(&mut self, platform: &Platform) -> Result<PageList, String> {
        let no_statements = platform.has_param("wpiu_no_statements");
        let sites = platform
            .get_param("wikidata_source_sites")
            .ok_or(format!("Missing parameter 'wikidata_source_sites'"))?;
        let sites: Vec<String> = sites.split(",").map(|s| s.to_string()).collect();
        if sites.is_empty() {
            return Err(format!("SourceWikidata: No wikidata source sites given"));
        }

        let sites = Platform::prep_quote(&sites);

        let mut sql = "SELECT ips_item_id FROM wb_items_per_site".to_string();
        if no_statements {
            sql += ",page_props,page";
        }
        sql += " WHERE ips_site_id IN (";
        sql += &sites.0;
        sql += ")";
        if no_statements {
            sql += " AND page_namespace=0 AND ips_item_id=substr(page_title,2)*1 AND page_id=pp_page AND pp_propname='wb-claims' AND pp_sortkey=0" ;
        }

        // Perform DB query
        let state = platform.state();
        let db_user_pass = match state.get_db_mutex().lock() {
            Ok(db) => db,
            Err(e) => return Err(format!("Bad mutex: {:?}", e)),
        };
        let mut conn = platform
            .state()
            .get_wiki_db_connection(&db_user_pass, &"wikidatawiki".to_string())?;
        let result = conn
            .prep_exec(sql, sites.1)
            .map_err(|e| format!("{:?}", e))?;
        let entries = result
            .filter_map(|row| row.ok())
            .filter_map(|row_inner| {
                let ips_item_id: usize = my::from_row(row_inner);
                let term_full_entity_id = format!("Q{}", ips_item_id);
                Platform::entry_from_entity(&term_full_entity_id)
            })
            .collect();
        Ok(PageList::new_from_vec(&"wikidatawiki".to_string(), entries))
    }
}

impl SourceWikidata {
    pub fn new() -> Self {
        Self {}
    }
}

//________________________________________________________________________________________________________________________

#[derive(Debug, Clone, PartialEq)]
pub struct SourcePagePile {}

impl DataSource for SourcePagePile {
    fn name(&self) -> String {
        "pagepile".to_string()
    }

    fn can_run(&self, platform: &Platform) -> bool {
        platform.has_param("pagepile")
    }

    fn run(&mut self, platform: &Platform) -> Result<PageList, String> {
        let pagepile = platform
            .get_param("pagepile")
            .ok_or(format!("Missing parameter 'pagepile'"))?;
        let timeout = Some(time::Duration::from_secs(240));
        let builder = reqwest::ClientBuilder::new().timeout(timeout);
        let api = Api::new_from_builder("https://www.wikidata.org/w/api.php", builder)
            .map_err(|e| e.to_string())?;
        let params = api.params_into(&vec![
            ("id", &pagepile.to_string()),
            ("action", "get_data"),
            ("format", "json"),
            ("doit", "1"),
        ]);
        let text = api
            .query_raw("https://tools.wmflabs.org/pagepile/api.php", &params, "GET")
            .map_err(|e| format!("PagePile: {:?}", e))?;
        let v: Value =
            serde_json::from_str(&text).map_err(|e| format!("PagePile JSON: {:?}", e))?;
        let wiki = v["wiki"]
            .as_str()
            .ok_or(format!("PagePile {} does not specify a wiki", &pagepile))?;
        let api = platform.state().get_api_for_wiki(wiki.to_string())?; // Just because we need query_raw
        let entries: Vec<PageListEntry> = v["pages"]
            .as_array()
            .ok_or(format!(
                "PagePile {} does not have a 'pages' array",
                &pagepile
            ))?
            .iter()
            .filter_map(|title| title.as_str())
            .map(|title| PageListEntry::new(Title::new_from_full(&title.to_string(), &api)))
            .collect();
        if entries.is_empty() {
            platform.warn(format!("<span tt='warn_pagepile'></span>"));
        }
        Ok(PageList::new_from_vec(wiki, entries))
    }
}

impl SourcePagePile {
    pub fn new() -> Self {
        Self {}
    }
}

//________________________________________________________________________________________________________________________

#[derive(Debug, Clone, PartialEq)]
pub struct SourceSearch {}

impl DataSource for SourceSearch {
    fn name(&self) -> String {
        "search".to_string()
    }

    fn can_run(&self, platform: &Platform) -> bool {
        platform.has_param("search_query")
            && platform.has_param("search_wiki")
            && platform.has_param("search_max_results")
    }

    fn run(&mut self, platform: &Platform) -> Result<PageList, String> {
        let wiki = platform
            .get_param("search_wiki")
            .ok_or(format!("Missing parameter 'search_wiki'"))?;
        let query = platform
            .get_param("search_query")
            .ok_or(format!("Missing parameter 'search_query'"))?;
        let max = match platform
            .get_param("search_max_results")
            .ok_or(format!("Missing parameter 'search_max_results'"))?
            .parse::<usize>()
        {
            Ok(max) => max,
            Err(e) => return Err(format!("{:?}", e)),
        };
        let api = platform.state().get_api_for_wiki(wiki.to_string())?;
        let srlimit = if max > 500 { 500 } else { max };
        let srlimit = format!("{}", srlimit);
        let params = api.params_into(&vec![
            ("action", "query"),
            ("list", "search"),
            ("srlimit", srlimit.as_str()),
            ("srsearch", query.as_str()),
        ]);
        let result = match api.get_query_api_json_limit(&params, Some(max)) {
            Ok(result) => result,
            Err(e) => return Err(format!("{:?}", e)),
        };
        let titles = Api::result_array_to_titles(&result);
        let entries = titles
            .iter()
            .map(|title| PageListEntry::new(title.to_owned()))
            .collect();
        let pagelist = PageList::new_from_vec(&wiki, entries);
        if pagelist.is_empty() {
            platform.warn(format!("<span tt='warn_search'></span>"));
        }
        Ok(pagelist)
    }
}

impl SourceSearch {
    pub fn new() -> Self {
        Self {}
    }
}

//________________________________________________________________________________________________________________________

#[derive(Debug, Clone, PartialEq)]
pub struct SourceManual {}

impl DataSource for SourceManual {
    fn name(&self) -> String {
        "manual".to_string()
    }

    fn can_run(&self, platform: &Platform) -> bool {
        platform.has_param("manual_list") && platform.has_param("manual_list_wiki")
    }

    fn run(&mut self, platform: &Platform) -> Result<PageList, String> {
        let wiki = platform
            .get_param("manual_list_wiki")
            .ok_or(format!("Missing parameter 'manual_list_wiki'"))?;
        let api = platform.state().get_api_for_wiki(wiki.to_string())?;
        let entries: Vec<PageListEntry> = platform
            .get_param("manual_list")
            .ok_or(format!("Missing parameter 'manual_list'"))?
            .split("\n")
            .filter_map(|line| {
                let line = line.trim().to_string();
                if !line.is_empty() {
                    let title = Title::new_from_full(&line, &api);
                    let entry = PageListEntry::new(title);
                    Some(entry)
                } else {
                    None
                }
            })
            .collect();
        let pagelist = PageList::new_from_vec(&wiki, entries);
        Ok(pagelist)
    }
}

impl SourceManual {
    pub fn new() -> Self {
        Self {}
    }
}

//________________________________________________________________________________________________________________________

#[derive(Debug, Clone, PartialEq)]
pub struct SourceSparql {}

impl DataSource for SourceSparql {
    fn name(&self) -> String {
        "sparql".to_string()
    }

    fn can_run(&self, platform: &Platform) -> bool {
        platform.has_param("sparql")
    }

    fn run(&mut self, platform: &Platform) -> Result<PageList, String> {
        let sparql = platform
            .get_param("sparql")
            .ok_or(format!("Missing parameter 'sparql'"))?;
        let timeout = Some(time::Duration::from_secs(120));
        let builder = reqwest::ClientBuilder::new().timeout(timeout);
        let api = Api::new_from_builder("https://www.wikidata.org/w/api.php", builder)
            .map_err(|e| format!("SourceSparql::run:1 {:?}", e))?;
        let result = api
            .sparql_query(sparql.as_str())
            .map_err(|e| format!("SourceSparql::run:2 {:?}", e))?;
        let first_var = result["head"]["vars"][0]
            .as_str()
            .ok_or(format!("No variables found in SPARQL result"))?;
        let ple: Vec<PageListEntry> = api
            .entities_from_sparql_result(&result, first_var)
            .par_iter()
            .filter_map(|e| Platform::entry_from_entity(e))
            .collect();
        if ple.is_empty() {
            platform.warn(format!("<span tt='warn_sparql'></span>"));
        }
        Ok(PageList::new_from_vec("wikidatawiki", ple))
    }
}

impl SourceSparql {
    pub fn new() -> Self {
        Self {}
    }
}
