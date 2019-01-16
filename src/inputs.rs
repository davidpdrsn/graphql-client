use deprecation::DeprecationStatus;
use failure;
use graphql_parser;
use heck::SnakeCase;
use introspection_response;
use objects::GqlObjectField;
use proc_macro2::{Ident, Span, TokenStream};
use query::QueryContext;
use schema::Schema;
use std::cell::Cell;
use std::collections::HashMap;

/// Represents an input object type from a GraphQL schema
#[derive(Debug, Clone, PartialEq)]
pub struct GqlInput<'schema> {
    pub description: Option<&'schema str>,
    pub name: &'schema str,
    pub fields: HashMap<&'schema str, GqlObjectField<'schema>>,
    pub is_required: Cell<bool>,
}

impl<'schema> GqlInput<'schema> {
    pub(crate) fn require(&self, schema: &Schema<'schema>) {
        if self.is_required.get() {
            return;
        }
        self.is_required.set(true);
        self.fields.values().for_each(|field| {
            schema.require(&field.type_.inner_name_str());
        })
    }

    pub(crate) fn to_rust(&self, context: &QueryContext) -> Result<TokenStream, failure::Error> {
        let name = Ident::new(&self.name, Span::call_site());
        let mut fields: Vec<&GqlObjectField> = self.fields.values().collect();
        fields.sort_unstable_by(|a, b| a.name.cmp(&b.name));
        let fields = fields.iter().map(|field| {
            let ty = field.type_.to_rust(&context, "");

            // If the type is recursive, we have to box it
            let ty = if field.type_.is_indirected() || field.type_.inner_name_str() != self.name {
                ty
            } else {
                quote! { Box<#ty> }
            };

            context.schema.require(&field.type_.inner_name_str());
            let original_name = &field.name;
            let snake_case_name = field.name.to_snake_case();
            let rename = ::shared::field_rename_annotation(&original_name, &snake_case_name);
            let name = Ident::new(&snake_case_name, Span::call_site());

            quote!(#rename pub #name: #ty)
        });
        let variables_derives = context.variables_derives();

        Ok(quote! {
            #variables_derives
            pub struct #name {
                #(#fields,)*
            }
        })
    }
}

impl<'schema> ::std::convert::From<&'schema graphql_parser::schema::InputObjectType>
    for GqlInput<'schema>
{
    fn from(schema_input: &'schema graphql_parser::schema::InputObjectType) -> GqlInput<'schema> {
        GqlInput {
            description: schema_input.description.as_ref().map(|s| s.as_str()),
            name: &schema_input.name,
            fields: schema_input
                .fields
                .iter()
                .map(|field| {
                    let name = field.name.as_str();
                    let field = GqlObjectField {
                        description: None,
                        name: &field.name,
                        type_: crate::field_type::FieldType::from(&field.value_type),
                        deprecation: DeprecationStatus::Current,
                    };
                    (name, field)
                })
                .collect(),
            is_required: false.into(),
        }
    }
}

impl<'schema> ::std::convert::From<&'schema introspection_response::FullType>
    for GqlInput<'schema>
{
    fn from(schema_input: &'schema introspection_response::FullType) -> GqlInput<'schema> {
        GqlInput {
            description: schema_input.description.as_ref().map(String::as_str),
            name: schema_input
                .name
                .as_ref()
                .map(String::as_str)
                .expect("unnamed input object"),
            fields: schema_input
                .input_fields
                .as_ref()
                .expect("fields on input object")
                .iter()
                .filter_map(|a| a.as_ref())
                .map(|f| {
                    let name = f
                        .input_value
                        .name
                        .as_ref()
                        .expect("unnamed input object field")
                        .as_str();
                    let field = GqlObjectField {
                        description: None,
                        name: &name,
                        type_: f
                            .input_value
                            .type_
                            .as_ref()
                            .map(|s| s.into())
                            .expect("type on input object field"),
                        deprecation: DeprecationStatus::Current,
                    };
                    (name, field)
                })
                .collect(),
            is_required: false.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use constants::*;
    use field_type::FieldType;

    #[test]
    fn gql_input_to_rust() {
        let cat = GqlInput {
            description: None,
            name: "Cat",
            fields: vec![
                (
                    "pawsCount",
                    GqlObjectField {
                        description: None,
                        name: "pawsCount",
                        type_: FieldType::Named(float_type()),
                        deprecation: DeprecationStatus::Current,
                    },
                ),
                (
                    "offsprings",
                    GqlObjectField {
                        description: None,
                        name: "offsprings",
                        type_: FieldType::Vector(Box::new(FieldType::Named("Cat"))),
                        deprecation: DeprecationStatus::Current,
                    },
                ),
                (
                    "requirements",
                    GqlObjectField {
                        description: None,
                        name: "requirements",
                        type_: FieldType::Optional(Box::new(FieldType::Named("CatRequirements"))),
                        deprecation: DeprecationStatus::Current,
                    },
                ),
            ]
            .into_iter()
            .collect(),
            is_required: false.into(),
        };

        let expected: String = vec![
            "# [ derive ( Serialize , Clone ) ] ",
            "pub struct Cat { ",
            "pub offsprings : Vec < Cat > , ",
            "# [ serde ( rename = \"pawsCount\" ) ] ",
            "pub paws_count : Float , ",
            "pub requirements : Option < CatRequirements > , ",
            "}",
        ]
        .into_iter()
        .collect();

        let mut schema = ::schema::Schema::new();
        schema.inputs.insert(cat.name, cat);
        let mut context = QueryContext::new_empty(&schema);
        context.ingest_additional_derives("Clone").unwrap();

        assert_eq!(
            format!(
                "{}",
                context.schema.inputs["Cat"].to_rust(&context).unwrap()
            ),
            expected
        );
    }
}
