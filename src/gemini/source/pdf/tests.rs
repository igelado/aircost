use super::*;
use crate::avionics::source::{exact_oem_product_identity_row, OemProductIdentity};
use lopdf::content::Operation;
use lopdf::{dictionary, Stream};

fn source_pdf(pages: &[&[&str]]) -> Vec<u8> {
    let mut document = Document::with_version("1.5");
    let pages_id = document.new_object_id();
    let font_id = document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Courier",
    });
    let resources_id = document.add_object(dictionary! {
        "Font" => dictionary! { "F1" => font_id },
    });
    let mut page_ids = Vec::new();
    for lines in pages {
        let mut operations = vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), 10.into()]),
            Operation::new("TL", vec![12.into()]),
            Operation::new("Td", vec![50.into(), 740.into()]),
        ];
        for line in *lines {
            operations.push(Operation::new("Tj", vec![Object::string_literal(*line)]));
            operations.push(Operation::new("T*", vec![]));
        }
        operations.push(Operation::new("ET", vec![]));
        let content_id = document.add_object(Stream::new(
            dictionary! {},
            Content { operations }.encode().unwrap(),
        ));
        page_ids.push(document.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => content_id,
        }));
    }
    finish_pdf(document, pages_id, resources_id, page_ids)
}

fn source_visual_row_pdf(rows: &[&[(i64, &str)]]) -> Vec<u8> {
    let mut document = Document::with_version("1.5");
    let pages_id = document.new_object_id();
    let font_id = document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Courier",
    });
    let resources_id = document.add_object(dictionary! {
        "Font" => dictionary! { "F1" => font_id },
    });
    let mut operations = vec![
        Operation::new("BT", vec![]),
        Operation::new("Tf", vec!["F1".into(), 10.into()]),
    ];
    for (row_index, row) in rows.iter().enumerate() {
        let y = 740_i64 - (row_index as i64 * 14);
        for (x, text) in *row {
            operations.push(Operation::new(
                "Tm",
                vec![
                    1.into(),
                    0.into(),
                    0.into(),
                    1.into(),
                    (*x).into(),
                    y.into(),
                ],
            ));
            operations.push(Operation::new("Tj", vec![Object::string_literal(*text)]));
        }
    }
    operations.push(Operation::new("ET", vec![]));
    let content_id = document.add_object(Stream::new(
        dictionary! {},
        Content { operations }.encode().unwrap(),
    ));
    let page_id = document.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "Contents" => content_id,
    });
    finish_pdf(document, pages_id, resources_id, vec![page_id])
}

fn source_text_operations_pdf(mut operations: Vec<Operation>, rotation: Option<i64>) -> Vec<u8> {
    let mut document = Document::with_version("1.5");
    let pages_id = document.new_object_id();
    let font_id = document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Courier",
    });
    let resources_id = document.add_object(dictionary! {
        "Font" => dictionary! { "F1" => font_id },
    });
    operations.insert(0, Operation::new("BT", vec![]));
    operations.insert(1, Operation::new("Tf", vec!["F1".into(), 10.into()]));
    operations.push(Operation::new("ET", vec![]));
    let content_id = document.add_object(Stream::new(
        dictionary! {},
        Content { operations }.encode().unwrap(),
    ));
    let mut page = dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "Contents" => content_id,
    };
    if let Some(rotation) = rotation {
        page.set("Rotate", rotation);
    }
    let page_id = document.add_object(page);
    finish_pdf(document, pages_id, resources_id, vec![page_id])
}

fn finish_pdf(
    mut document: Document,
    pages_id: ObjectId,
    resources_id: ObjectId,
    page_ids: Vec<ObjectId>,
) -> Vec<u8> {
    document.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => page_ids.iter().copied().map(Object::Reference).collect::<Vec<_>>(),
            "Count" => page_ids.len() as i64,
            "Resources" => resources_id,
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        }),
    );
    let catalog_id = document.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    document.trailer.set("Root", catalog_id);
    let mut bytes = Vec::new();
    document.save_to(&mut bytes).unwrap();
    bytes
}

#[test]
fn extracts_bounded_generic_text_and_physical_fragments() {
    let pdf = source_pdf(&[
        &[
            "GEA 71 Unit (011-00831-00) 010-00283-00",
            "GEA 71 Unit Rack 115-00411-00",
        ],
        &["GEA 71 Installation Manual"],
    ]);
    let extracted = extract(&pdf, None).unwrap();

    assert!(extracted
        .publisher_text
        .contains("GEA 71 Unit (011-00831-00) 010-00283-00"));
    assert_eq!(extracted.source_text_rows.len(), 3);
    assert!(extracted.source_text_rows_complete);
    assert!(extracted
        .source_text_rows
        .iter()
        .all(|row| row.kind == TextRowKind::PdfPhysicalLine));
}

#[test]
fn resource_limits_and_encryption_fail_closed() {
    assert!(extract(b"%PDF-not-a-document", None).is_err());
    assert!(extract(&source_pdf(&[&[]]), None).is_err());

    let two_pages = source_pdf(&[&["Garmin GIA 63"], &["Garmin GIA 63W"]]);
    assert!(extract_with_limits(
        &two_pages,
        Limits {
            max_pages: 1,
            ..LIMITS
        },
        None,
    )
    .is_err());
    assert!(extract_with_limits(
        &two_pages,
        Limits {
            max_total_text_bytes: 4,
            ..LIMITS
        },
        None,
    )
    .is_err());

    let pdf = source_pdf(&[&["Garmin GIA 63W"]]);
    let mut document = Document::load_mem(&pdf).unwrap();
    let encrypt_id = document.add_object(dictionary! {
        "Filter" => "Standard",
        "V" => 1,
        "R" => 2,
        "Length" => 40,
        "O" => Object::string_literal("owner"),
        "U" => Object::string_literal("user"),
        "P" => -4,
    });
    document.trailer.set("Encrypt", encrypt_id);
    let mut encrypted = Vec::new();
    document.save_to(&mut encrypted).unwrap();
    assert!(extract(&encrypted, None).is_err());
}

#[test]
fn targeted_projection_ignores_unrelated_noise_but_not_target_overflow() {
    let target = ProductIdentityTarget::new("GEA 71", "011-00831-00").unwrap();
    let oversized = "unrelated ".repeat(MAX_TEXT_ROW_CHARACTERS);
    let mut crowded = vec!["unrelated"; MAX_TEXT_ROWS + 1];
    crowded.push("GEA 71 Unit (011-00831-00) 010-00283-00");
    let pdf = source_pdf(&[&[oversized.as_str()], crowded.as_slice()]);

    assert!(!extract(&pdf, None).unwrap().source_text_rows_complete);
    let targeted = extract(&pdf, Some(&target)).unwrap();
    assert!(targeted.source_text_rows_complete);
    assert_eq!(targeted.source_text_rows.len(), 1);
    assert_eq!(
        targeted.source_text_rows[0].text,
        "GEA 71 Unit (011-00831-00) 010-00283-00"
    );

    let oversized_target = format!(
        "GEA 71 011-00831-00 {}",
        "target filler ".repeat(MAX_TEXT_ROW_CHARACTERS)
    );
    assert!(extract(&source_pdf(&[&[&oversized_target]]), Some(&target)).is_err());
}

#[test]
fn visual_rows_reconstruct_only_one_page_and_baseline() {
    let target = ProductIdentityTarget::new("ME406", "453-6603").unwrap();
    let pdf = source_visual_row_pdf(&[
        &[(40, "ME406 (453-6603),"), (220, "ME406HM (453-6604)")],
        &[
            (40, "Emergency Locator Transmitter"),
            (260, "453-6603"),
            (380, "ME406"),
        ],
        &[
            (40, "Emergency Locator Transmitter"),
            (260, "453-6604"),
            (380, "ME406HM"),
        ],
    ]);
    let extracted = extract(&pdf, Some(&target)).unwrap();

    assert!(extracted.source_text_rows_complete);
    assert_eq!(extracted.source_text_rows.len(), 2);
    let target_identity = OemProductIdentity {
        catalog_id: 125,
        model: "ME406",
        manufacturer_identifier: "453-6603",
    };
    let neighbor_identity = OemProductIdentity {
        catalog_id: 126,
        model: "ME406HM",
        manufacturer_identifier: "453-6604",
    };
    assert_eq!(
        exact_oem_product_identity_row(
            &extracted.source_text_rows,
            extracted.source_text_rows_complete,
            target_identity,
            &[target_identity, neighbor_identity],
        )
        .unwrap(),
        "Emergency Locator Transmitter 453-6603 ME406"
    );

    let split_pdf = source_visual_row_pdf(&[&[(40, "ME406")], &[(260, "453-6603")]]);
    let split = extract(&split_pdf, Some(&target)).unwrap();
    assert!(exact_oem_product_identity_row(
        &split.source_text_rows,
        split.source_text_rows_complete,
        target_identity,
        &[target_identity, neighbor_identity],
    )
    .is_err());
}

#[test]
fn scaled_rotated_text_displacements_use_the_displayed_page_baseline() {
    let operations = vec![
        Operation::new(
            "Tm",
            vec![
                0.into(),
                (-2).into(),
                2.into(),
                0.into(),
                100.into(),
                700.into(),
            ],
        ),
        Operation::new("Tj", vec![Object::string_literal("GSU 75")]),
        Operation::new("Td", vec![100.into(), 0.into()]),
        Operation::new("Tj", vec![Object::string_literal("010-01127-00")]),
        Operation::new("Td", vec![(-100).into(), (-7).into()]),
        Operation::new("Tj", vec![Object::string_literal("GSU 75H")]),
        Operation::new("Td", vec![100.into(), 0.into()]),
        Operation::new("Tj", vec![Object::string_literal("010-01127-20")]),
    ];
    let target = ProductIdentityTarget::new("GSU 75", "010-01127-00").unwrap();
    let target_identity = OemProductIdentity {
        catalog_id: 734,
        model: "GSU 75",
        manufacturer_identifier: "010-01127-00",
    };
    let neighbor_identity = OemProductIdentity {
        catalog_id: 735,
        model: "GSU 75H",
        manufacturer_identifier: "010-01127-20",
    };

    let rotated = source_text_operations_pdf(operations.clone(), Some(90));
    let extracted = extract(&rotated, Some(&target)).unwrap();
    assert_eq!(
        exact_oem_product_identity_row(
            &extracted.source_text_rows,
            extracted.source_text_rows_complete,
            target_identity,
            &[target_identity, neighbor_identity],
        )
        .unwrap(),
        "GSU 75 010-01127-00"
    );
    assert!(!extracted
        .source_text_rows
        .iter()
        .any(|row| { row.text.contains("010-01127-00") && row.text.contains("010-01127-20") }));

    let not_display_horizontal = source_text_operations_pdf(operations, None);
    assert!(extract(&not_display_horizontal, Some(&target)).is_err());
}

#[test]
fn missing_invoked_fonts_fail_closed() {
    let mut missing_font =
        Document::load_mem(&source_visual_row_pdf(&[&[(40, "GEA 71 011-00831-00")]])).unwrap();
    let page_id = *missing_font.get_pages().values().next().unwrap();
    let pages_id = missing_font
        .get_dictionary(page_id)
        .unwrap()
        .get(b"Parent")
        .unwrap()
        .as_reference()
        .unwrap();
    missing_font
        .get_dictionary_mut(pages_id)
        .unwrap()
        .set("Resources", Object::Dictionary(dictionary! {}));
    let mut bytes = Vec::new();
    missing_font.save_to(&mut bytes).unwrap();
    let target = ProductIdentityTarget::new("GEA 71", "011-00831-00").unwrap();
    assert!(extract(&bytes, Some(&target)).is_err());
}

#[test]
fn text_form_with_own_resources_and_matrix_reconstructs_a_garmin_style_row() {
    let mut document = Document::with_version("1.5");
    let pages_id = document.new_object_id();
    let form_font = document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Courier",
    });
    let form = Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 500.into(), 40.into()],
            "Matrix" => vec![1.into(), 0.into(), 0.into(), 1.into(), 40.into(), 700.into()],
            "Resources" => dictionary! {
                "Font" => dictionary! { "FForm" => form_font },
            },
        },
        Content {
            operations: vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec![Object::Name(b"FForm".to_vec()), 10.into()]),
                Operation::new(
                    "Tm",
                    vec![1.into(), 0.into(), 0.into(), 1.into(), 5.into(), 10.into()],
                ),
                Operation::new(
                    "Tj",
                    vec![Object::string_literal("GIA 63W Unit Only 011-01105-00")],
                ),
                Operation::new("ET", vec![]),
            ],
        }
        .encode()
        .unwrap(),
    );
    let form_id = document.add_object(form);
    let resources_id = document.add_object(dictionary! {
        "Font" => dictionary! {
            "FForm" => dictionary! { "Type" => "NotAFont" },
        },
        "XObject" => dictionary! { "TargetForm" => form_id },
    });
    let content_id = document.add_object(Stream::new(
        dictionary! {},
        Content {
            operations: vec![Operation::new(
                "Do",
                vec![Object::Name(b"TargetForm".to_vec())],
            )],
        }
        .encode()
        .unwrap(),
    ));
    let page_id = document.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "Contents" => content_id,
    });
    let bytes = finish_pdf(document, pages_id, resources_id, vec![page_id]);
    let target = ProductIdentityTarget::new("GIA 63W", "011-01105-00").unwrap();
    let extracted = extract(&bytes, Some(&target)).unwrap();
    assert_eq!(
        extracted.source_text_rows[0].text,
        "GIA 63W Unit Only 011-01105-00"
    );
}

#[test]
fn nested_form_uses_page_resources_only_when_its_resources_are_absent() {
    let mut document = Document::with_version("1.5");
    let pages_id = document.new_object_id();
    let page_font = document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Courier",
    });
    let inner_form = document.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 400.into(), 30.into()],
            "Matrix" => vec![1.into(), 0.into(), 0.into(), 1.into(), 20.into(), 10.into()],
        },
        Content {
            operations: vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec![Object::Name(b"FPage".to_vec()), 10.into()]),
                Operation::new(
                    "Tm",
                    vec![1.into(), 0.into(), 0.into(), 1.into(), 5.into(), 5.into()],
                ),
                Operation::new("Tj", vec![Object::string_literal("GTX 345R 011-03378-40")]),
                Operation::new("ET", vec![]),
            ],
        }
        .encode()
        .unwrap(),
    ));
    let outer_form = document.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 500.into(), 80.into()],
            "Matrix" => vec![1.into(), 0.into(), 0.into(), 1.into(), 20.into(), 650.into()],
            "Resources" => dictionary! {
                "XObject" => dictionary! { "Inner" => inner_form },
            },
        },
        Content {
            operations: vec![Operation::new("Do", vec![Object::Name(b"Inner".to_vec())])],
        }
        .encode()
        .unwrap(),
    ));
    let resources_id = document.add_object(dictionary! {
        "Font" => dictionary! { "FPage" => page_font },
        "XObject" => dictionary! { "Outer" => outer_form },
    });
    let content_id = document.add_object(Stream::new(
        dictionary! {},
        Content {
            operations: vec![Operation::new("Do", vec![Object::Name(b"Outer".to_vec())])],
        }
        .encode()
        .unwrap(),
    ));
    let page_id = document.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "Contents" => content_id,
    });
    let bytes = finish_pdf(document, pages_id, resources_id, vec![page_id]);
    let target = ProductIdentityTarget::new("GTX 345R", "011-03378-40").unwrap();
    let extracted = extract(&bytes, Some(&target)).unwrap();
    assert_eq!(extracted.source_text_rows[0].text, "GTX 345R 011-03378-40");
}

#[test]
fn form_graphics_and_font_state_do_not_escape_the_invocation() {
    let mut document = Document::with_version("1.5");
    let pages_id = document.new_object_id();
    let page_font = document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Courier",
    });
    let form_font = document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });
    let form = document.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 100.into(), 100.into()],
            "Matrix" => vec![2.into(), 0.into(), 0.into(), 2.into(), 200.into(), 100.into()],
            "Resources" => dictionary! {
                "Font" => dictionary! { "FForm" => form_font },
            },
        },
        Content {
            operations: vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec![Object::Name(b"FForm".to_vec()), 6.into()]),
                Operation::new("TL", vec![40.into()]),
                Operation::new("ET", vec![]),
            ],
        }
        .encode()
        .unwrap(),
    ));
    let resources_id = document.add_object(dictionary! {
        "Font" => dictionary! { "FPage" => page_font },
        "XObject" => dictionary! { "Stateful" => form },
    });
    let content_id = document.add_object(Stream::new(
        dictionary! {},
        Content {
            operations: vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec![Object::Name(b"FPage".to_vec()), 10.into()]),
                Operation::new("TL", vec![12.into()]),
                Operation::new("ET", vec![]),
                Operation::new("Do", vec![Object::Name(b"Stateful".to_vec())]),
                Operation::new("BT", vec![]),
                Operation::new(
                    "Tm",
                    vec![
                        1.into(),
                        0.into(),
                        0.into(),
                        1.into(),
                        40.into(),
                        700.into(),
                    ],
                ),
                Operation::new("Tj", vec![Object::string_literal("GEA 71 011-00831-00")]),
                Operation::new("ET", vec![]),
            ],
        }
        .encode()
        .unwrap(),
    ));
    let page_id = document.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "Contents" => content_id,
    });
    let bytes = finish_pdf(document, pages_id, resources_id, vec![page_id]);
    let target = ProductIdentityTarget::new("GEA 71", "011-00831-00").unwrap();
    let extracted = extract(&bytes, Some(&target)).unwrap();
    assert_eq!(extracted.source_text_rows[0].text, "GEA 71 011-00831-00");
}

#[test]
fn text_outside_a_form_bbox_cannot_establish_a_product_row() {
    let mut document = Document::with_version("1.5");
    let pages_id = document.new_object_id();
    let font = document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Courier",
    });
    let form = document.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 10.into(), 10.into()],
            "Resources" => dictionary! {
                "Font" => dictionary! { "F1" => font },
            },
        },
        Content {
            operations: vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec![Object::Name(b"F1".to_vec()), 10.into()]),
                Operation::new(
                    "Tm",
                    vec![1.into(), 0.into(), 0.into(), 1.into(), 20.into(), 20.into()],
                ),
                Operation::new("Tj", vec![Object::string_literal("GTX 345R 011-03378-40")]),
                Operation::new("ET", vec![]),
            ],
        }
        .encode()
        .unwrap(),
    ));
    let resources_id = document.add_object(dictionary! {
        "XObject" => dictionary! { "Clipped" => form },
    });
    let content_id = document.add_object(Stream::new(
        dictionary! {},
        Content {
            operations: vec![Operation::new(
                "Do",
                vec![Object::Name(b"Clipped".to_vec())],
            )],
        }
        .encode()
        .unwrap(),
    ));
    let page_id = document.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "Contents" => content_id,
    });
    let bytes = finish_pdf(document, pages_id, resources_id, vec![page_id]);
    let target = ProductIdentityTarget::new("GTX 345R", "011-03378-40").unwrap();
    assert!(extract(&bytes, Some(&target)).is_err());
}

#[test]
fn recursive_form_cycles_fail_closed_but_repeated_invocation_is_allowed() {
    let mut cyclic = Document::with_version("1.5");
    let cyclic_pages = cyclic.new_object_id();
    let form_id = cyclic.new_object_id();
    cyclic.objects.insert(
        form_id,
        Object::Stream(Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => "Form",
                "BBox" => vec![0.into(), 0.into(), 100.into(), 100.into()],
                "Resources" => dictionary! {
                    "XObject" => dictionary! { "Self" => form_id },
                },
            },
            Content {
                operations: vec![Operation::new("Do", vec![Object::Name(b"Self".to_vec())])],
            }
            .encode()
            .unwrap(),
        )),
    );
    let cyclic_resources = cyclic.add_object(dictionary! {
        "XObject" => dictionary! { "Cycle" => form_id },
    });
    let cyclic_content = cyclic.add_object(Stream::new(
        dictionary! {},
        Content {
            operations: vec![Operation::new("Do", vec![Object::Name(b"Cycle".to_vec())])],
        }
        .encode()
        .unwrap(),
    ));
    let cyclic_page = cyclic.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => cyclic_pages,
        "Contents" => cyclic_content,
    });
    let cyclic_bytes = finish_pdf(cyclic, cyclic_pages, cyclic_resources, vec![cyclic_page]);
    let target = ProductIdentityTarget::new("GEA 71", "011-00831-00").unwrap();
    let cycle_error = match extract(&cyclic_bytes, Some(&target)) {
        Ok(_) => panic!("a recursive Form XObject cycle must fail"),
        Err(error) => error,
    };
    assert!(cycle_error.to_string().contains("cycle"));

    let mut repeated = Document::with_version("1.5");
    let repeated_pages = repeated.new_object_id();
    let font = repeated.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Courier",
    });
    let form = repeated.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 300.into(), 30.into()],
            "Resources" => dictionary! {
                "Font" => dictionary! { "F1" => font },
            },
        },
        Content {
            operations: vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec![Object::Name(b"F1".to_vec()), 10.into()]),
                Operation::new(
                    "Tm",
                    vec![1.into(), 0.into(), 0.into(), 1.into(), 5.into(), 10.into()],
                ),
                Operation::new("Tj", vec![Object::string_literal("GEA 71 011-00831-00")]),
                Operation::new("ET", vec![]),
            ],
        }
        .encode()
        .unwrap(),
    ));
    let repeated_resources = repeated.add_object(dictionary! {
        "XObject" => dictionary! { "Repeated" => form },
    });
    let repeated_content = repeated.add_object(Stream::new(
        dictionary! {},
        Content {
            operations: vec![
                Operation::new("Do", vec![Object::Name(b"Repeated".to_vec())]),
                Operation::new("q", vec![]),
                Operation::new(
                    "cm",
                    vec![
                        1.into(),
                        0.into(),
                        0.into(),
                        1.into(),
                        0.into(),
                        (-40).into(),
                    ],
                ),
                Operation::new("Do", vec![Object::Name(b"Repeated".to_vec())]),
                Operation::new("Q", vec![]),
            ],
        }
        .encode()
        .unwrap(),
    ));
    let repeated_page = repeated.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => repeated_pages,
        "Contents" => repeated_content,
    });
    let repeated_bytes = finish_pdf(
        repeated,
        repeated_pages,
        repeated_resources,
        vec![repeated_page],
    );
    let extracted = extract(&repeated_bytes, Some(&target)).unwrap();
    assert_eq!(extracted.source_text_rows.len(), 2);
}

#[test]
fn malformed_invoked_form_contracts_fail_closed() {
    fn malformed_form_pdf(mut dictionary: lopdf::Dictionary, do_operands: Vec<Object>) -> Vec<u8> {
        let mut document = Document::with_version("1.5");
        let pages_id = document.new_object_id();
        dictionary.set("Type", "XObject");
        let form = document.add_object(Stream::new(
            dictionary,
            Content {
                operations: vec![Operation::new("m", vec![0.into(), 0.into()])],
            }
            .encode()
            .unwrap(),
        ));
        let resources = document.add_object(dictionary! {
            "XObject" => dictionary! { "Malformed" => form },
        });
        let content = document.add_object(Stream::new(
            dictionary! {},
            Content {
                operations: vec![Operation::new("Do", do_operands)],
            }
            .encode()
            .unwrap(),
        ));
        let page = document.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => content,
        });
        finish_pdf(document, pages_id, resources, vec![page])
    }

    let target = ProductIdentityTarget::new("GEA 71", "011-00831-00").unwrap();
    let malformed = [
        dictionary! {
            "Subtype" => "PS",
            "BBox" => vec![0.into(), 0.into(), 10.into(), 10.into()],
        },
        dictionary! {
            "Subtype" => "Form",
            "FormType" => 2,
            "BBox" => vec![0.into(), 0.into(), 10.into(), 10.into()],
        },
        dictionary! {
            "Subtype" => "Form",
            "BBox" => vec![
                Object::string_literal("left"),
                0.into(),
                10.into(),
                10.into(),
            ],
        },
        dictionary! {
            "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 10.into(), 10.into()],
            "Matrix" => vec![1.into(), 0.into(), 0.into()],
        },
        dictionary! {
            "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 10.into(), 10.into()],
            "Resources" => 7,
        },
        dictionary! {
            "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 10.into(), 10.into()],
            "Ref" => dictionary! {},
        },
    ];
    for dictionary in malformed {
        assert!(extract(
            &malformed_form_pdf(dictionary, vec![Object::Name(b"Malformed".to_vec())],),
            Some(&target),
        )
        .is_err());
    }
    assert!(extract(
        &malformed_form_pdf(
            dictionary! {
                "Subtype" => "Form",
                "BBox" => vec![0.into(), 0.into(), 10.into(), 10.into()],
            },
            vec![Object::Name(b"Malformed".to_vec()), 1.into()],
        ),
        Some(&target),
    )
    .is_err());
}

#[test]
fn nearest_resource_names_shadow_ancestors_and_unused_entries_are_ignored() {
    let target = ProductIdentityTarget::new("GEA 71", "011-00831-00").unwrap();

    let mut document = Document::with_version("1.5");
    let pages_id = document.new_object_id();
    let font_id = document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Courier",
    });
    let inherited_image = document.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => 1,
            "Height" => 1,
            "ColorSpace" => "DeviceGray",
            "BitsPerComponent" => 8,
        },
        vec![0],
    ));
    let direct_text_form = document.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 100.into(), 100.into()],
        },
        Content {
            operations: vec![
                Operation::new("BT", vec![]),
                Operation::new("Tj", vec![Object::string_literal("untracked text")]),
                Operation::new("ET", vec![]),
            ],
        }
        .encode()
        .unwrap(),
    ));
    let parent_resources = document.add_object(dictionary! {
        "Font" => dictionary! { "F1" => font_id },
        "XObject" => dictionary! { "Proof" => inherited_image },
    });
    let direct_resources = document.add_object(dictionary! {
        "Font" => dictionary! { "F1" => font_id },
        "XObject" => dictionary! { "Proof" => direct_text_form },
    });
    let content_id = document.add_object(Stream::new(
        dictionary! {},
        Content {
            operations: vec![Operation::new("Do", vec![Object::Name(b"Proof".to_vec())])],
        }
        .encode()
        .unwrap(),
    ));
    let page_id = document.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "Resources" => direct_resources,
        "Contents" => content_id,
    });
    let bytes = finish_pdf(document, pages_id, parent_resources, vec![page_id]);
    assert!(extract(&bytes, Some(&target)).is_err());

    let mut document = Document::with_version("1.5");
    let pages_id = document.new_object_id();
    let valid_font = document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Courier",
    });
    let inherited_invalid_font = document.add_object(dictionary! { "Type" => "NotAFont" });
    let inherited_text_form = document.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 100.into(), 100.into()],
        },
        Content {
            operations: vec![Operation::new(
                "Tj",
                vec![Object::string_literal("untracked text")],
            )],
        }
        .encode()
        .unwrap(),
    ));
    let direct_image = document.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => 1,
            "Height" => 1,
            "ColorSpace" => "DeviceGray",
            "BitsPerComponent" => 8,
        },
        vec![0],
    ));
    let unused_text_form = document.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 100.into(), 100.into()],
        },
        Content {
            operations: vec![Operation::new(
                "Tj",
                vec![Object::string_literal("unused text")],
            )],
        }
        .encode()
        .unwrap(),
    ));
    let inherited_resources = document.add_object(dictionary! {
        "Font" => dictionary! { "F1" => inherited_invalid_font },
        "XObject" => dictionary! { "Proof" => inherited_text_form },
    });
    let direct_resources = document.add_object(dictionary! {
        "Font" => dictionary! {
            "F1" => valid_font,
            "UnusedInvalidFont" => dictionary! { "Type" => "NotAFont" },
        },
        "XObject" => dictionary! {
            "Proof" => direct_image,
            "UnusedTextForm" => unused_text_form,
        },
    });
    let content_id = document.add_object(Stream::new(
        dictionary! {},
        Content {
            operations: vec![
                Operation::new("Do", vec![Object::Name(b"Proof".to_vec())]),
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec![Object::Name(b"F1".to_vec()), 10.into()]),
                Operation::new(
                    "Tm",
                    vec![
                        1.into(),
                        0.into(),
                        0.into(),
                        1.into(),
                        40.into(),
                        700.into(),
                    ],
                ),
                Operation::new("Tj", vec![Object::string_literal("GEA 71 011-00831-00")]),
                Operation::new("ET", vec![]),
            ],
        }
        .encode()
        .unwrap(),
    ));
    let page_id = document.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "Resources" => direct_resources,
        "Contents" => content_id,
    });
    let bytes = finish_pdf(document, pages_id, inherited_resources, vec![page_id]);
    let extracted = extract(&bytes, Some(&target)).unwrap();
    assert_eq!(extracted.source_text_rows[0].text, "GEA 71 011-00831-00");
}

#[test]
fn page_resources_shadow_the_whole_inherited_dictionary() {
    let mut document = Document::with_version("1.5");
    let pages_id = document.new_object_id();
    let font = document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Courier",
    });
    let inherited = document.add_object(dictionary! {
        "Font" => dictionary! { "FAncestor" => font },
    });
    let content = document.add_object(Stream::new(
        dictionary! {},
        Content {
            operations: vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec![Object::Name(b"FAncestor".to_vec()), 10.into()]),
                Operation::new(
                    "Tm",
                    vec![
                        1.into(),
                        0.into(),
                        0.into(),
                        1.into(),
                        40.into(),
                        700.into(),
                    ],
                ),
                Operation::new("Tj", vec![Object::string_literal("GEA 71 011-00831-00")]),
                Operation::new("ET", vec![]),
            ],
        }
        .encode()
        .unwrap(),
    ));
    let page = document.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "Resources" => dictionary! {},
        "Contents" => content,
    });
    let pdf = finish_pdf(document, pages_id, inherited, vec![page]);
    let target = ProductIdentityTarget::new("GEA 71", "011-00831-00").unwrap();
    assert!(extract(&pdf, Some(&target)).is_err());
}

#[test]
fn every_declared_page_content_stream_must_decode_strictly() {
    let target = ProductIdentityTarget::new("GEA 71", "011-00831-00").unwrap();
    let base = source_visual_row_pdf(&[&[(40, "GEA 71 011-00831-00")]]);

    let mut unsupported = Document::load_mem(&base).unwrap();
    let page = *unsupported.get_pages().values().next().unwrap();
    let content = unsupported
        .get_dictionary(page)
        .unwrap()
        .get(b"Contents")
        .unwrap()
        .as_reference()
        .unwrap();
    unsupported
        .get_object_mut(content)
        .unwrap()
        .as_stream_mut()
        .unwrap()
        .dict
        .set("Filter", "UnsupportedDecode");
    let mut bytes = Vec::new();
    unsupported.save_to(&mut bytes).unwrap();
    assert!(extract(&bytes, Some(&target)).is_err());

    let mut malformed_filter = Document::load_mem(&base).unwrap();
    let page = *malformed_filter.get_pages().values().next().unwrap();
    let content = malformed_filter
        .get_dictionary(page)
        .unwrap()
        .get(b"Contents")
        .unwrap()
        .as_reference()
        .unwrap();
    malformed_filter
        .get_object_mut(content)
        .unwrap()
        .as_stream_mut()
        .unwrap()
        .dict
        .set("Filter", 7);
    let mut bytes = Vec::new();
    malformed_filter.save_to(&mut bytes).unwrap();
    assert!(extract(&bytes, Some(&target)).is_err());

    let mut mixed = Document::load_mem(&base).unwrap();
    let page = *mixed.get_pages().values().next().unwrap();
    let valid = mixed
        .get_dictionary(page)
        .unwrap()
        .get(b"Contents")
        .unwrap()
        .as_reference()
        .unwrap();
    let broken = mixed.add_object(Stream::new(
        dictionary! { "Filter" => "UnsupportedDecode" },
        b"broken".to_vec(),
    ));
    mixed
        .get_dictionary_mut(page)
        .unwrap()
        .set("Contents", vec![valid.into(), broken.into()]);
    let mut bytes = Vec::new();
    mixed.save_to(&mut bytes).unwrap();
    assert!(extract(&bytes, Some(&target)).is_err());

    let mut deep = Document::load_mem(&base).unwrap();
    let page = *deep.get_pages().values().next().unwrap();
    let mut content = deep
        .get_dictionary(page)
        .unwrap()
        .get(b"Contents")
        .unwrap()
        .as_reference()
        .unwrap();
    for _ in 0..=MAX_CONTENT_OBJECT_DEPTH {
        content = deep.add_object(Object::Reference(content));
    }
    deep.get_dictionary_mut(page)
        .unwrap()
        .set("Contents", content);
    let mut bytes = Vec::new();
    deep.save_to(&mut bytes).unwrap();
    assert!(extract(&bytes, Some(&target)).is_err());
}

#[test]
fn malformed_explicit_font_mappings_fail_instead_of_falling_back() {
    let mut document =
        Document::load_mem(&source_visual_row_pdf(&[&[(40, "GEA 71 011-00831-00")]])).unwrap();
    let page = *document.get_pages().values().next().unwrap();
    let pages = document
        .get_dictionary(page)
        .unwrap()
        .get(b"Parent")
        .unwrap()
        .as_reference()
        .unwrap();
    let resources = document
        .get_dictionary(pages)
        .unwrap()
        .get(b"Resources")
        .unwrap()
        .as_reference()
        .unwrap();
    let font = document
        .get_dictionary(resources)
        .unwrap()
        .get(b"Font")
        .unwrap()
        .as_dict()
        .unwrap()
        .get(b"F1")
        .unwrap()
        .as_reference()
        .unwrap();
    let malformed_cmap = document.add_object(Stream::new(dictionary! {}, b"not a CMap".to_vec()));
    document
        .get_dictionary_mut(font)
        .unwrap()
        .set("ToUnicode", malformed_cmap);
    let mut pdf = Vec::new();
    document.save_to(&mut pdf).unwrap();
    let target = ProductIdentityTarget::new("GEA 71", "011-00831-00").unwrap();
    assert!(extract(&pdf, Some(&target)).is_err());

    let mut type_three =
        Document::load_mem(&source_visual_row_pdf(&[&[(40, "GEA 71 011-00831-00")]])).unwrap();
    let page = *type_three.get_pages().values().next().unwrap();
    let pages = type_three
        .get_dictionary(page)
        .unwrap()
        .get(b"Parent")
        .unwrap()
        .as_reference()
        .unwrap();
    let resources = type_three
        .get_dictionary(pages)
        .unwrap()
        .get(b"Resources")
        .unwrap()
        .as_reference()
        .unwrap();
    let font = type_three
        .get_dictionary(resources)
        .unwrap()
        .get(b"Font")
        .unwrap()
        .as_dict()
        .unwrap()
        .get(b"F1")
        .unwrap()
        .as_reference()
        .unwrap();
    type_three
        .get_dictionary_mut(font)
        .unwrap()
        .set("Subtype", "Type3");
    let mut pdf = Vec::new();
    type_three.save_to(&mut pdf).unwrap();
    assert!(extract(&pdf, Some(&target)).is_err());

    let font_dictionary = type_three.get_dictionary_mut(font).unwrap();
    font_dictionary.set("Subtype", "TrueType");
    font_dictionary.set("BaseFont", "ABCDEF+CustomFont");
    let mut pdf = Vec::new();
    type_three.save_to(&mut pdf).unwrap();
    assert!(extract(&pdf, Some(&target)).is_err());

    let font_dictionary = type_three.get_dictionary_mut(font).unwrap();
    font_dictionary.set("Subtype", "Type1");
    font_dictionary.set("BaseFont", "Courier");
    font_dictionary.set(
        "Encoding",
        dictionary! {
            "Type" => "Encoding",
            "Differences" => vec![0.into(), Object::Name(b"space".to_vec())],
        },
    );
    let mut pdf = Vec::new();
    type_three.save_to(&mut pdf).unwrap();
    assert!(extract(&pdf, Some(&target)).is_err());

    for differences in [
        vec![Object::Name(b"G".to_vec()), Object::Name(b"E".to_vec())],
        vec![
            255.into(),
            Object::Name(b"A".to_vec()),
            Object::Name(b"B".to_vec()),
        ],
    ] {
        type_three.get_dictionary_mut(font).unwrap().set(
            "Encoding",
            dictionary! {
                "Type" => "Encoding",
                "BaseEncoding" => "StandardEncoding",
                "Differences" => differences,
            },
        );
        let mut pdf = Vec::new();
        type_three.save_to(&mut pdf).unwrap();
        assert!(extract(&pdf, Some(&target)).is_err());
    }

    let mut type_zero =
        Document::load_mem(&source_visual_row_pdf(&[&[(40, "GEA 71 011-00831-00")]])).unwrap();
    let page = *type_zero.get_pages().values().next().unwrap();
    let pages = type_zero
        .get_dictionary(page)
        .unwrap()
        .get(b"Parent")
        .unwrap()
        .as_reference()
        .unwrap();
    let resources = type_zero
        .get_dictionary(pages)
        .unwrap()
        .get(b"Resources")
        .unwrap()
        .as_reference()
        .unwrap();
    let font = type_zero
        .get_dictionary(resources)
        .unwrap()
        .get(b"Font")
        .unwrap()
        .as_dict()
        .unwrap()
        .get(b"F1")
        .unwrap()
        .as_reference()
        .unwrap();
    let font_dictionary = type_zero.get_dictionary_mut(font).unwrap();
    font_dictionary.set("Subtype", "Type0");
    font_dictionary.set("Encoding", "WinAnsiEncoding");
    font_dictionary.set("FirstChar", 0);
    font_dictionary.set(
        "Widths",
        (0..=255).map(|_| Object::Integer(1)).collect::<Vec<_>>(),
    );
    font_dictionary.set(
        "FontDescriptor",
        dictionary! {
            "Type" => "FontDescriptor",
            "FontBBox" => vec![0.into(), 0.into(), 1.into(), 1.into()],
        },
    );
    let content = type_zero
        .get_dictionary(page)
        .unwrap()
        .get(b"Contents")
        .unwrap()
        .as_reference()
        .unwrap();
    type_zero
        .get_object_mut(content)
        .unwrap()
        .as_stream_mut()
        .unwrap()
        .set_content(
            Content {
                operations: vec![
                    Operation::new("re", vec![0.into(), 0.into(), 20.into(), 30.into()]),
                    Operation::new("W", vec![]),
                    Operation::new("n", vec![]),
                    Operation::new("BT", vec![]),
                    Operation::new("Tf", vec![Object::Name(b"F1".to_vec()), 10.into()]),
                    Operation::new(
                        "Tm",
                        vec![1.into(), 0.into(), 0.into(), 1.into(), 5.into(), 10.into()],
                    ),
                    Operation::new("Tj", vec![Object::string_literal("GEA 71 011-00831-00")]),
                    Operation::new("ET", vec![]),
                ],
            }
            .encode()
            .unwrap(),
        );
    let mut pdf = Vec::new();
    type_zero.save_to(&mut pdf).unwrap();
    assert!(extract(&pdf, Some(&target)).is_err());
}

#[test]
fn text_advancement_rise_and_full_clip_extents_fail_closed() {
    fn form_pdf(operations: Vec<Operation>, bbox: Vec<Object>) -> Vec<u8> {
        let mut document = Document::with_version("1.5");
        let pages_id = document.new_object_id();
        let font = document.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Courier",
        });
        let form = document.add_object(Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => "Form",
                "BBox" => bbox,
                "Resources" => dictionary! {
                    "Font" => dictionary! { "F1" => font },
                },
            },
            Content { operations }.encode().unwrap(),
        ));
        let resources = document.add_object(dictionary! {
            "XObject" => dictionary! { "Proof" => form },
        });
        let content = document.add_object(Stream::new(
            dictionary! {},
            Content {
                operations: vec![Operation::new("Do", vec![Object::Name(b"Proof".to_vec())])],
            }
            .encode()
            .unwrap(),
        ));
        let page = document.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => content,
        });
        finish_pdf(document, pages_id, resources, vec![page])
    }

    let target = ProductIdentityTarget::new("GEA 71", "011-00831-00").unwrap();
    let prefix = vec![
        Operation::new("BT", vec![]),
        Operation::new("Tf", vec![Object::Name(b"F1".to_vec()), 10.into()]),
        Operation::new(
            "Tm",
            vec![1.into(), 0.into(), 0.into(), 1.into(), 5.into(), 10.into()],
        ),
    ];
    let mut advanced = prefix.clone();
    advanced.push(Operation::new(
        "Tj",
        vec![Object::string_literal("x".repeat(50))],
    ));
    advanced.push(Operation::new(
        "Tj",
        vec![Object::string_literal("GEA 71 011-00831-00")],
    ));
    advanced.push(Operation::new("ET", vec![]));
    assert!(extract(
        &form_pdf(advanced, vec![0.into(), 0.into(), 300.into(), 30.into()]),
        Some(&target),
    )
    .is_err());

    let mut risen = prefix.clone();
    risen.push(Operation::new("Ts", vec![30.into()]));
    risen.push(Operation::new(
        "Tj",
        vec![Object::string_literal("GEA 71 011-00831-00")],
    ));
    risen.push(Operation::new("ET", vec![]));
    assert!(extract(
        &form_pdf(risen, vec![0.into(), 0.into(), 200.into(), 20.into()]),
        Some(&target),
    )
    .is_err());

    let mut narrow = prefix;
    narrow.push(Operation::new(
        "Tj",
        vec![Object::string_literal("GEA 71 011-00831-00")],
    ));
    narrow.push(Operation::new("ET", vec![]));
    assert!(extract(
        &form_pdf(narrow, vec![0.into(), 0.into(), 20.into(), 30.into()]),
        Some(&target),
    )
    .is_err());
}

#[test]
fn rectangle_clips_keep_the_ctm_from_path_construction() {
    let operations = vec![
        Operation::new("re", vec![0.into(), 0.into(), 150.into(), 30.into()]),
        Operation::new(
            "cm",
            vec![1.into(), 0.into(), 0.into(), 1.into(), 200.into(), 0.into()],
        ),
        Operation::new("W", vec![]),
        Operation::new("n", vec![]),
        Operation::new(
            "Tm",
            vec![1.into(), 0.into(), 0.into(), 1.into(), 5.into(), 10.into()],
        ),
        Operation::new("Tj", vec![Object::string_literal("GEA 71 011-00831-00")]),
    ];
    let target = ProductIdentityTarget::new("GEA 71", "011-00831-00").unwrap();
    assert!(extract(&source_text_operations_pdf(operations, None), Some(&target)).is_err());
}

#[test]
fn malformed_text_show_operands_are_rejected() {
    let malformed = [
        Operation::new(
            "Tj",
            vec![
                Object::string_literal("GEA 71"),
                Object::string_literal("extra"),
            ],
        ),
        Operation::new(
            "TJ",
            vec![Object::Array(vec![
                Object::string_literal("GEA 71"),
                Object::Name(b"bad".to_vec()),
            ])],
        ),
        Operation::new(
            "'",
            vec![
                Object::string_literal("GEA 71"),
                Object::string_literal("extra"),
            ],
        ),
        Operation::new("\"", vec![0.into(), 0.into(), Object::Array(Vec::new())]),
    ];
    let target = ProductIdentityTarget::new("GEA 71", "011-00831-00").unwrap();
    for operation in malformed {
        assert!(extract(
            &source_text_operations_pdf(
                vec![
                    Operation::new(
                        "Tm",
                        vec![
                            1.into(),
                            0.into(),
                            0.into(),
                            1.into(),
                            40.into(),
                            700.into(),
                        ],
                    ),
                    operation,
                ],
                None,
            ),
            Some(&target),
        )
        .is_err());
    }
}

#[test]
fn user_unit_controls_physical_baseline_grouping() {
    let mut document = Document::with_version("1.5");
    let pages_id = document.new_object_id();
    let font = document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Courier",
    });
    let resources = document.add_object(dictionary! {
        "Font" => dictionary! { "F1" => font },
    });
    let content = document.add_object(Stream::new(
        dictionary! {},
        Content {
            operations: vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec![Object::Name(b"F1".to_vec()), 10.into()]),
                Operation::new(
                    "Tm",
                    vec![
                        1.into(),
                        0.into(),
                        0.into(),
                        1.into(),
                        40.into(),
                        100.into(),
                    ],
                ),
                Operation::new("Tj", vec![Object::string_literal("GEA 71")]),
                Operation::new(
                    "Tm",
                    vec![
                        1.into(),
                        0.into(),
                        0.into(),
                        1.into(),
                        140.into(),
                        Object::Real(99.7),
                    ],
                ),
                Operation::new("Tj", vec![Object::string_literal("011-00831-00")]),
                Operation::new("ET", vec![]),
            ],
        }
        .encode()
        .unwrap(),
    ));
    let page = document.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "UserUnit" => 2,
        "Contents" => content,
    });
    let pdf = finish_pdf(document, pages_id, resources, vec![page]);
    let target = ProductIdentityTarget::new("GEA 71", "011-00831-00").unwrap();
    let extracted = extract(&pdf, Some(&target)).unwrap();
    assert!(!extracted
        .source_text_rows
        .iter()
        .any(|row| { row.text.contains("GEA 71") && row.text.contains("011-00831-00") }));

    let mut inherited = Document::load_mem(&pdf).unwrap();
    let page = *inherited.get_pages().values().next().unwrap();
    let parent = inherited
        .get_dictionary(page)
        .unwrap()
        .get(b"Parent")
        .unwrap()
        .as_reference()
        .unwrap();
    inherited
        .get_dictionary_mut(page)
        .unwrap()
        .remove(b"UserUnit");
    inherited
        .get_dictionary_mut(parent)
        .unwrap()
        .set("UserUnit", 2);
    let mut pdf = Vec::new();
    inherited.save_to(&mut pdf).unwrap();
    let extracted = extract(&pdf, Some(&target)).unwrap();
    assert!(extracted
        .source_text_rows
        .iter()
        .any(|row| { row.text.contains("GEA 71") && row.text.contains("011-00831-00") }));
}

#[test]
fn page_tree_validation_rejects_invalid_and_overdeep_sibling_branches() {
    fn save(mut document: Document, root_pages: ObjectId) -> Vec<u8> {
        let catalog = document.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => root_pages,
        });
        document.trailer.set("Root", catalog);
        let mut bytes = Vec::new();
        document.save_to(&mut bytes).unwrap();
        bytes
    }

    let target = ProductIdentityTarget::new("GEA 71", "011-00831-00").unwrap();
    let mut invalid = Document::with_version("1.5");
    let root = invalid.new_object_id();
    let empty_content = invalid.add_object(Stream::new(dictionary! {}, Vec::new()));
    let page = invalid.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => root,
        "Contents" => empty_content,
    });
    invalid.objects.insert(
        root,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![page.into(), Object::Reference((999_999, 0))],
            "Count" => 2,
        }),
    );
    assert!(extract(&save(invalid, root), Some(&target)).is_err());

    let mut deep = Document::with_version("1.5");
    let root = deep.new_object_id();
    let mut nodes = vec![root];
    for _ in 0..=MAX_PAGE_TREE_DEPTH {
        nodes.push(deep.new_object_id());
    }
    for index in 0..nodes.len() - 1 {
        let parent = nodes[index];
        let child = nodes[index + 1];
        let mut dictionary = dictionary! {
            "Type" => "Pages",
            "Kids" => vec![child.into()],
            "Count" => 1,
        };
        if index > 0 {
            dictionary.set("Parent", nodes[index - 1]);
        }
        deep.objects.insert(parent, Object::Dictionary(dictionary));
    }
    let parent = *nodes.last().unwrap();
    let empty_content = deep.add_object(Stream::new(dictionary! {}, Vec::new()));
    deep.objects.insert(
        parent,
        Object::Dictionary(dictionary! {
            "Type" => "Page",
            "Parent" => nodes[nodes.len() - 2],
            "Contents" => empty_content,
        }),
    );
    assert!(extract(&save(deep, root), Some(&target)).is_err());
}

#[test]
fn deterministic_content_parsing_rejects_trailing_malformed_syntax() {
    let mut document =
        Document::load_mem(&source_visual_row_pdf(&[&[(40, "GEA 71 011-00831-00")]])).unwrap();
    let page = *document.get_pages().values().next().unwrap();
    let content = document
        .get_dictionary(page)
        .unwrap()
        .get(b"Contents")
        .unwrap()
        .as_reference()
        .unwrap();
    document
        .get_object_mut(content)
        .unwrap()
        .as_stream_mut()
        .unwrap()
        .content = b"BT /F1 10 Tf 1 0 0 1 40 700 Tm (GEA 71 011-00831-00) Tj ET [".to_vec();
    let mut pdf = Vec::new();
    document.save_to(&mut pdf).unwrap();
    let target = ProductIdentityTarget::new("GEA 71", "011-00831-00").unwrap();
    assert!(extract(&pdf, Some(&target)).is_err());

    let mut document = Document::with_version("1.5");
    let pages = document.new_object_id();
    let font = document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Courier",
    });
    let form = document.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 200.into(), 30.into()],
            "Resources" => dictionary! {
                "Font" => dictionary! { "F1" => font },
            },
        },
        b"BT /F1 10 Tf 1 0 0 1 5 10 Tm (GEA 71 011-00831-00) Tj ET [".to_vec(),
    ));
    let resources = document.add_object(dictionary! {
        "XObject" => dictionary! { "Proof" => form },
    });
    let content = document.add_object(Stream::new(
        dictionary! {},
        Content {
            operations: vec![Operation::new("Do", vec![Object::Name(b"Proof".to_vec())])],
        }
        .encode()
        .unwrap(),
    ));
    let page = document.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages,
        "Contents" => content,
    });
    let pdf = finish_pdf(document, pages, resources, vec![page]);
    assert!(extract(&pdf, Some(&target)).is_err());
}

#[test]
fn invoked_font_and_form_count_and_decompression_budgets_fail_closed() {
    fn resource_heavy_pdf(font_count: usize, form_contents: &[Vec<u8>]) -> Vec<u8> {
        let mut document = Document::with_version("1.5");
        let pages_id = document.new_object_id();
        let mut fonts = lopdf::Dictionary::new();
        let mut operations = Vec::new();
        for index in 0..font_count {
            let font_id = document.add_object(dictionary! {
                "Type" => "Font",
                "Subtype" => "Type1",
                "BaseFont" => "Courier",
            });
            let name = format!("F{index}");
            fonts.set(name.as_bytes(), font_id);
            operations.push(Operation::new(
                "Tf",
                vec![Object::Name(name.into_bytes()), 10.into()],
            ));
        }
        let mut xobjects = lopdf::Dictionary::new();
        for (index, form_content) in form_contents.iter().enumerate() {
            let name = format!("X{index}");
            let form_id = document.add_object(Stream::new(
                dictionary! {
                    "Type" => "XObject",
                    "Subtype" => "Form",
                    "BBox" => vec![0.into(), 0.into(), 100.into(), 100.into()],
                },
                form_content.clone(),
            ));
            xobjects.set(name.as_bytes(), form_id);
            operations.push(Operation::new("Do", vec![Object::Name(name.into_bytes())]));
        }
        operations.extend([
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec![Object::Name(b"F0".to_vec()), 10.into()]),
            Operation::new(
                "Tm",
                vec![
                    1.into(),
                    0.into(),
                    0.into(),
                    1.into(),
                    40.into(),
                    700.into(),
                ],
            ),
            Operation::new("Tj", vec![Object::string_literal("ME406 453-6603")]),
            Operation::new("ET", vec![]),
        ]);
        let resources_id = document.add_object(dictionary! {
            "Font" => fonts,
            "XObject" => xobjects,
        });
        let content_id = document.add_object(Stream::new(
            dictionary! {},
            Content { operations }.encode().unwrap(),
        ));
        let page_id = document.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => content_id,
        });
        finish_pdf(document, pages_id, resources_id, vec![page_id])
    }

    let target = ProductIdentityTarget::new("ME406", "453-6603").unwrap();
    let too_many_fonts = resource_heavy_pdf(MAX_INVOKED_FONTS_PER_PAGE + 1, &[]);
    assert!(extract(&too_many_fonts, Some(&target)).is_err());

    let empty_form = Content {
        operations: vec![Operation::new("m", vec![0.into(), 0.into()])],
    }
    .encode()
    .unwrap();
    let too_many_forms =
        resource_heavy_pdf(1, &vec![empty_form; MAX_INVOKED_FORM_XOBJECTS_PER_PAGE + 1]);
    assert!(extract(&too_many_forms, Some(&target)).is_err());

    let large_form = b"0 0 m\n".repeat(100);
    let cumulative_form_overflow = resource_heavy_pdf(1, &[large_form.clone(), large_form]);
    assert!(extract_with_limits(
        &cumulative_form_overflow,
        Limits {
            max_page_decompressed_bytes: 1_024,
            ..LIMITS
        },
        Some(&target),
    )
    .is_err());

    fn repeated_or_nested_form_pdf(repetitions: usize, depth: usize) -> Vec<u8> {
        let mut document = Document::with_version("1.5");
        let pages_id = document.new_object_id();
        let mut child = None;
        for _ in 0..depth.max(1) {
            let mut form_dictionary = dictionary! {
                "Type" => "XObject",
                "Subtype" => "Form",
                "BBox" => vec![0.into(), 0.into(), 10.into(), 10.into()],
            };
            let operations = if let Some(child_id) = child {
                form_dictionary.set(
                    "Resources",
                    dictionary! {
                        "XObject" => dictionary! { "Child" => child_id },
                    },
                );
                vec![Operation::new("Do", vec![Object::Name(b"Child".to_vec())])]
            } else {
                vec![Operation::new("m", vec![0.into(), 0.into()])]
            };
            child = Some(document.add_object(Stream::new(
                form_dictionary,
                Content { operations }.encode().unwrap(),
            )));
        }
        let root = child.unwrap();
        let resources = document.add_object(dictionary! {
            "XObject" => dictionary! { "Root" => root },
        });
        let content = document.add_object(Stream::new(
            dictionary! {},
            Content {
                operations: (0..repetitions)
                    .map(|_| Operation::new("Do", vec![Object::Name(b"Root".to_vec())]))
                    .collect::<Vec<_>>(),
            }
            .encode()
            .unwrap(),
        ));
        let page = document.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => content,
        });
        finish_pdf(document, pages_id, resources, vec![page])
    }

    let invocation_overflow = repeated_or_nested_form_pdf(MAX_XOBJECT_INVOCATIONS_PER_PAGE + 1, 1);
    let invocation_error = match extract(&invocation_overflow, Some(&target)) {
        Ok(_) => panic!("excessive repeated Form invocation must fail"),
        Err(error) => error,
    };
    assert!(invocation_error.to_string().contains("too many XObjects"));

    let depth_overflow = repeated_or_nested_form_pdf(1, MAX_FORM_XOBJECT_DEPTH + 2);
    let depth_error = match extract(&depth_overflow, Some(&target)) {
        Ok(_) => panic!("excessive nested Form depth must fail"),
        Err(error) => error,
    };
    assert!(depth_error.to_string().contains("depth"));
}

#[test]
#[ignore = "requires manually downloaded official OEM PDF fixtures"]
fn downloaded_official_oem_pdf_regressions() {
    let directory =
        std::env::var("AIRCOST_OEM_PDF_FIXTURE_DIR").expect("set AIRCOST_OEM_PDF_FIXTURE_DIR");
    let cases = [
        ("gdu1040.pdf", 3, "GDU 1040", "011-00972-00", None),
        ("gea71.pdf", 30, "GEA 71", "011-00831-00", None),
        (
            "me406.pdf",
            125,
            "ME406",
            "453-6603",
            Some((126, "ME406HM", "453-6604")),
        ),
        ("gea71b.pdf", 244, "GEA 71B", "011-03682-00", None),
        ("gsu75.pdf", 734, "GSU 75", "010-01127-00", None),
    ];
    for (file, catalog_id, model, identifier, neighbor) in cases {
        let pdf = std::fs::read(std::path::Path::new(&directory).join(file))
            .unwrap_or_else(|error| panic!("could not read {file}: {error}"));
        let target = ProductIdentityTarget::new(model, identifier).unwrap();
        let extracted = extract(&pdf, Some(&target))
            .unwrap_or_else(|error| panic!("{file} extraction failed: {error}"));
        let target_identity = OemProductIdentity {
            catalog_id,
            model,
            manufacturer_identifier: identifier,
        };
        let mut catalog = vec![target_identity];
        if let Some((catalog_id, model, manufacturer_identifier)) = neighbor {
            catalog.push(OemProductIdentity {
                catalog_id,
                model,
                manufacturer_identifier,
            });
        }
        exact_oem_product_identity_row(
            &extracted.source_text_rows,
            extracted.source_text_rows_complete,
            target_identity,
            &catalog,
        )
        .unwrap_or_else(|error| {
            panic!(
                "{file} did not verify: {error}; retained target rows: {:?}",
                extracted.source_text_rows
            )
        });
    }
}

#[test]
#[ignore = "requires the manually downloaded official Garmin G1000 NXi PDF fixture"]
fn downloaded_g1000_nxi_text_form_regression() {
    use sha2::{Digest, Sha256};

    let path =
        std::env::var("AIRCOST_G1000_NXI_PDF_FIXTURE").expect("set AIRCOST_G1000_NXI_PDF_FIXTURE");
    let pdf = std::fs::read(&path)
        .unwrap_or_else(|error| panic!("could not read G1000 NXi fixture {path}: {error}"));
    assert_eq!(
        format!("{:x}", Sha256::digest(&pdf)),
        "e68021d03d5533286fbc2aa1b1903195ee2b34fe09cffec297e5990c1064402f",
        "the local regression fixture changed"
    );
    let target = ProductIdentityTarget::new("G1000 NXi", "G1000 NXi").unwrap();
    let extracted = extract(&pdf, Some(&target))
        .unwrap_or_else(|error| panic!("G1000 NXi Form extraction failed: {error}"));
    let identity = OemProductIdentity {
        catalog_id: 262,
        model: "G1000 NXi",
        manufacturer_identifier: "G1000 NXi",
    };
    exact_oem_product_identity_row(
            &extracted.source_text_rows,
            extracted.source_text_rows_complete,
            identity,
            &[identity],
        )
        .unwrap_or_else(|error| {
            panic!(
                "G1000 NXi Form extraction did not preserve one exact product row: {error}; retained rows: {:?}",
                extracted.source_text_rows
            )
        });
}
