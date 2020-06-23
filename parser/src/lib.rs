extern crate proc_macro;
use ethabi::{Contract, Event};
use inflector::cases::snakecase::to_snake_case;
use itertools::Itertools;
use proc_macro::TokenStream;
use proc_macro2::Span;
use quote::quote;
use rustc_hex::ToHex;
use serde::{Deserialize, Serialize};
use serde_syn::{config, from_stream};
use std::fs::File;
use std::io::{BufReader, Error, ErrorKind};
use syn::{
    self,
    parse::{Parse, ParseStream},
    Ident,
};
use tiny_keccak;

#[derive(Debug, Default, Clone, Deserialize, Serialize)]
struct Input {
    #[serde(default = "default_event_names")]
    event_names: Vec<String>,
    abi_path: String,
}

fn default_event_names() -> Vec<String> {
    Vec::new()
}

impl Parse for Input {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        from_stream(config::ANYTHING_GOES, input)
    }
}

#[proc_macro]
pub fn generate_event_parsers(tokens: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(tokens as Input);
    let abi_path = std::fs::canonicalize(std::path::Path::new(&input.abi_path)).unwrap();
    // let comment = format!("// Generating event parsers from abi: {:?}", abi_path);
    // let comment_quote : TokenStream = (quote! { #comment }).into();

    let abi =
        get_abi_from_file(abi_path.as_path()).expect(format!("Error reading file: {:?}", abi_path).as_str());
    let mut ts = TokenStream::new();
    // ts.extend(comment_quote);

    for event_entry in abi.events.iter() {
        let event_list = event_entry.1;
        let event = &event_list[0];

        if is_included(&event.name, &input.event_names) {
            ts.extend(generate_event(&event));
        }
    }
    ts
}

fn is_included(event_name: &String, event_name_list: &Vec<String>) -> bool {
    if !event_name_list.is_empty() {
        event_name_list.contains(event_name)
    } else {
        true
    }
}

fn get_abi_from_file(file_path: &std::path::Path) -> Result<Contract, Error> {
    let file = File::open(file_path)?;
    let reader = BufReader::new(file);
    Contract::load(reader).map_err(|e| Error::new(ErrorKind::Other, e.to_string()))
}

fn generate_event(event: &Event) -> TokenStream {
    let event_name = Ident::new(&event.name, Span::call_site());
    let event_name_string = event.name.clone();

    let params: Vec<proc_macro2::TokenStream> = event
        .inputs
        .clone()
        .iter()
        .map(|param| {
            let var_name = Ident::new(&to_snake_case(&param.name.to_string()), Span::call_site());
            let var_type = solidity_to_rust_type(format!("{}", param.kind).as_str());
            quote! { #var_name: #var_type , }
        })
        .collect();
    let params_quote = quote! {#(#params)*};

    let event_params: Vec<proc_macro2::TokenStream> = event
        .inputs
        .iter()
        .map(|param| {
            let param_name = param.name.clone();
            let param_kind = solidity_to_event_param_type(format!("{}", param.kind).as_str());
            let param_indexed =
                Ident::new(format!("{}", param.indexed).as_str(), Span::call_site());
            quote! {
               ::ethabi::EventParam {
                   name: #param_name.to_owned(),
                   kind: #param_kind,
                   indexed: #param_indexed,
               }
            }
        })
        .collect();
    let event_params_quote = quote! {#(#event_params,)*};

    let parse_params: Vec<proc_macro2::TokenStream> = event
        .inputs
        .iter()
        .map(|p| {
            let param_name = p.name.clone();
            let var_name = Ident::new(&to_snake_case(&p.name.to_string()), Span::call_site());
            let parse_logic = solidity_to_parse_param_type(format!("{}", p.kind).as_str());
            quote! {
                #param_name => out.#var_name = p.value.to_owned().#parse_logic,
            }
        })
        .collect();
    let parse_params_quote = quote! {#(#parse_params)*};

    let param_types: Vec<String> = event.inputs.iter().map(|p| format!("{}", p.kind)).collect();
    let event_sig_params = param_types.iter().format(",");
    let event_sig = format!("{}({})", &event.name, event_sig_params);
    // println!("event_sig: {}", event_sig);
    let event_sig_hash = generate_keccak256(event_sig.as_str());

    let event_name_uppercase = to_snake_case(&event.name).to_uppercase();
    let static_event_desc = Ident::new(
        format!("{}_EVENT_DESC", event_name_uppercase).as_str(),
        Span::call_site(),
    );

    (quote! {
        ::lazy_static::lazy_static! {
            static ref #static_event_desc: ::ethabi::Event = {
                ::ethabi::Event {
                    name: #event_name_string.to_owned(),
                    inputs: vec![
                        #event_params_quote
                    ],
                    anonymous: false,
                }
            };
        }
        // generating struct
        #[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
        pub struct #event_name {
            #params_quote
        }

        impl web3_eth_event_parser::traits::ChainEventParser for #event_name {
            fn event_name() -> String {
                #event_name_string.to_string()
            }
            fn event_hash() -> String {
                #event_sig_hash.to_string()
            }
            fn parse_event(log: &::web3::types::Log) -> Result<Self, String> {
                let raw_log = ::ethabi::RawLog {
                    topics: log.topics.clone(),
                    data: log.data.clone().0,
                };

                let ethabi_log = #static_event_desc.parse_log(raw_log).map_err(|e| e.to_string())?;
                let mut out = #event_name {..Default::default()};
                // parse parameters
                ethabi_log
                    .params
                    .iter()
                    .for_each(|p| match p.name.as_ref() {
                        #parse_params_quote
                        _ => (),
                    });
                Ok(out)
            }
        }
    })
    .into()
}

fn solidity_to_parse_param_type(string: &str) -> proc_macro2::TokenStream {
    match string {
        "string" => quote! { to_string().unwrap_or_default() },
        "bool" => quote! { to_bool().unwrap_or_default() },
        "address" => quote! { to_address().unwrap_or_default() },
        "uint256" | "uint128" | "uint64" | "uint32" | "uint8" => quote! { to_uint().unwrap_or_default() },
        "int256" | "int128" | "int64" | "int32" | "int8" => quote! { to_int().unwrap_or_default() },
        "bytes32" => quote! { to_fixed_bytes().unwrap_or_default() },
        "uint256[]" => quote! { to_array().unwrap_or_default().iter().map(|x| x.to_owned().to_uint().unwrap_or_default()).collect() },
        _ => panic!("Invalid type: {}", string),
    }
}

fn solidity_to_event_param_type(string: &str) -> proc_macro2::TokenStream {
    match string {
        "string" => quote! { ::ethabi::ParamType::String },
        "bool" => quote! { ::ethabi::ParamType::Bool },
        "address" => quote! { ::ethabi::ParamType::Address },
        "uint256" => quote! { ::ethabi::ParamType::Uint(256) },
        "uint128" => quote! { ::ethabi::ParamType::Uint(128) },
        "uint64" => quote! { ::ethabi::ParamType::Uint(64) },
        "uint32" => quote! { ::ethabi::ParamType::Uint(32) },
        "uint8" => quote! { ::ethabi::ParamType::Uint(8) },
        "int256" => quote! { ::ethabi::ParamType::Int(256) },
        "int128" => quote! { ::ethabi::ParamType::Int(128) },
        "int64" => quote! { ::ethabi::ParamType::Int(64) },
        "int32" => quote! { ::ethabi::ParamType::Int(32) },
        "int8" => quote! { ::ethabi::ParamType::Int(8) },
        "bytes32" => quote! { ::ethabi::ParamType::FixedBytes(32) },
        "uint256[]" => quote! { ::ethabi::ParamType::Array(Box::new(::ethabi::ParamType::Uint(256))) },
        _ => panic!("Invalid type: {}", string),
    }
}

fn solidity_to_rust_type(string: &str) -> proc_macro2::TokenStream {
    match string {
        "string" => quote! { ::std::string::String },
        "bool" => quote! { bool },
        "address" => quote! { ::web3::types::Address },
        "uint256" => quote! { ::web3::types::U256 },
        "uint128" => quote! { u128 },
        "uint64" => quote! { u64 },
        "uint32" => quote! { u32 },
        "uint8" => quote! { u8 },
        "int256" => quote! { ::web3::types::U256 },
        "int128" => quote! { i128 },
        "int64" => quote! { i64 },
        "int32" => quote! { i32 },
        "int8" => quote! { i8 },
        "bytes32" => quote! { Vec<u8> },
        "uint256[]" => quote! { Vec<::web3::types::U256> },
        _ => panic!("Invalid type: {}", string),
    }
}

fn generate_keccak256(value: &str) -> String {
    let result = tiny_keccak::keccak256(value.as_ref());
    let mut hex_string = "".to_owned();
    hex_string.push_str(result.to_hex::<String>().as_ref());
    hex_string
}
