use super::*;

#[test]
fn scalar_words_match_preserved_objects() {
    for (module, address, source) in [
        ("ADJUST", 0x1a0, "movk 1,a0"),
        ("ADJUST", 0x1e20, "addk 15,a10"),
        ("ADJUST", 0x2c90, "subk 5,a10"),
        ("ADJUST", 0x4e30, "mpyu a0,a1"),
        ("ADJUST", 0x5990, "divu a1,a2"),
        ("ATTRACT", 0x1d30, "neg a11"),
        ("ATTRACT", 0xb860, "zext a0"),
        ("ATTRACT", 0x2f790, "mpys a0,a1"),
        ("ATTRACT", 0x2f7a0, "divs a2,a1"),
        ("AUDIT", 0x1240, "dint"),
        ("AUDIT", 0x9510, "clrc"),
        ("AUDIT", 0x9560, "setc"),
        ("BAKGND", 0x120, "subxy a9,a3"),
        ("BAKGND", 0x2c0, "addxy a9,a11"),
        ("BAKGND", 0x2f0, "cmpxy a11,a1"),
        ("BAKGND", 0xab0, "sext a0"),
        ("BB", 0x10, "setf 16,1,0"),
        ("BB3", 0xf540, "abs a0"),
        ("DIAG", 0x48f0, "modu a3,a2"),
        ("DIAG", 0x7ab0, "getst a3"),
        ("DIAG", 0xab60, "exgf a9,1"),
        ("DIAG", 0x15c00, "cvxyl a3,a3"),
        ("DIAG", 0x15ca0, "drav a2,a0"),
        ("DIAG", 0x16060, "line 0"),
        ("DIAG", 0x160d0, "line 1"),
        ("MAIN", 0x3e80, "lmo a2,a0"),
    ] {
        let data = fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("../BACKUP/XCODE101L/{module}.OBJ")),
        )
        .unwrap();
        let text = u32::from_le_bytes(data[40..44].try_into().unwrap()) as usize;
        let mut assembler = Assembler::new();
        let ast = assembler.pass_one(&[source]).unwrap();
        assert!(omitted_instructions(&ast).is_empty(), "{source}");
        assert_eq!(
            assembler.pass_two(&ast).unwrap(),
            data[text + address / 8..text + address / 8 + 2],
            "{source}"
        );
    }
}

#[test]
fn immediate_words_and_fields_match_execution_semantics() {
    for (source, words) in [
        ("getpc b7", vec![0x0157u16]),
        ("abs b14", vec![0x039e]),
        ("neg b3", vec![0x03b3]),
        ("addc b2,b3", vec![0x4253]),
        ("subb a2,a3", vec![0x4643]),
        ("sext b2,1", vec![0x0712]),
        ("zext a2,1", vec![0x0722]),
        ("setf 8,0", vec![0x0548]),
        ("setf 32,1,1", vec![0x0760]),
        ("addk 32,b3", vec![0x1013]),
        ("subk 1,b3", vec![0x1433]),
        ("movk 32,a3", vec![0x1803]),
        ("addi 12345678h,b3", vec![0x0b33, 0x5678, 0x1234]),
        ("subi 1,a2,W", vec![0x0be2, 0xfffe]),
        ("subi 12345678h,b3", vec![0x0d13, 0xa987, 0xedcb]),
        ("cmpi -1,b2,W", vec![0x0b52, 0]),
        ("cmpi 12345678h,a3", vec![0x0b63, 0xa987, 0xedcb]),
        ("andi 12345678h,a3", vec![0x0b83, 0xa987, 0xedcb]),
        ("andni 12345678h,a3", vec![0x0b83, 0x5678, 0x1234]),
        ("ori 12345678h,b3", vec![0x0bb3, 0x5678, 0x1234]),
        ("xori 12345678h,a3", vec![0x0bc3, 0x5678, 0x1234]),
        ("pixblt B,XY", vec![0x0fa0]),
        ("movi 4<<10+4<<5+7,a0", vec![0x09e0, 0x1087, 0]),
        (
            "movi [0F2H,166H]-[41H,29H],a4",
            vec![0x09e4, 0x013d, 0x00b1],
        ),
        ("movi ',',a2", vec![0x09e2, 0x002c, 0]),
    ] {
        let mut assembler = Assembler::new();
        let ast = assembler.pass_one(&[source]).unwrap();
        assert!(omitted_instructions(&ast).is_empty(), "{source}");
        assert_eq!(
            assembler.pass_two(&ast).unwrap(),
            words
                .into_iter()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>(),
            "{source}"
        );
    }
    for source in [
        "addk 0,a0",
        "subk 33,a0",
        "movk -1,a0",
        "abs 3",
        "divu a2,b3",
        "setf 33,0",
        "setf 8,2",
        "sext a2,2",
        "line 2",
        "pixblt B,B",
    ] {
        let mut assembler = Assembler::new();
        let ast = assembler.pass_one(&[source]).unwrap();
        assert!(assembler.pass_two(&ast).is_err(), "{source}");
    }
    for source in ["addi 1,a2 trailing", "getpc a0,a1", "subi ,a0", "setf 8"] {
        assert!(Assembler::new().pass_one(&[source]).is_err(), "{source}");
    }
}

#[test]
fn deferred_immediates_freeze_mutable_aliases_and_relocate_complements() {
    let source=".ref outside\n.asg 3/8,FRACTION\n.eval 1,N\n.word N+outside\naddi 16*FRACTION,a0\nsubi N+outside,b3\n.eval 2,N\n.asg 1/2,FRACTION\naddi 16*FRACTION,a0\nsubi N+outside,b3\n.word N+outside";
    let mut assembler = Assembler::new();
    let ast = assembler
        .pass_one(&source.lines().collect::<Vec<_>>())
        .unwrap();
    let binary = assembler.pass_two(&ast).unwrap();
    assert_eq!(&binary[4..8], &6u32.to_le_bytes());
    assert_eq!(&binary[16..20], &8u32.to_le_bytes());
    let object = ObjectFile::from_assembler(Path::new("test.asm"), &assembler, &ast, &binary);
    assert!(object
        .fixups
        .iter()
        .any(|f| f.symbol == "(~((1)+outside))&0FFFFFFFFh" && f.offset == 10));
    assert!(object
        .fixups
        .iter()
        .any(|f| f.symbol == "(~((2)+outside))&0FFFFFFFFh" && f.offset == 22));
    assert!(object
        .fixups
        .iter()
        .any(|f| f.symbol == "(1)+outside" && f.offset == 0));
    assert!(object
        .fixups
        .iter()
        .any(|f| f.symbol == "(2)+outside" && f.offset == 26));
}

#[test]
fn mutable_equates_and_bit_address_table_counts_are_preserved() {
    let source=".eval 0,N\nA .equ N\n.eval N+1,N\nB .equ N\ntable .long 0,0,0\nCOUNT .equ ($-table)/32\nmovk COUNT,a0\nWIDTH .equ 31\nmovk WIDTH,b3";
    let mut assembler = Assembler::new();
    let ast = assembler
        .pass_one(&source.lines().collect::<Vec<_>>())
        .unwrap();
    assert_eq!(assembler.symbol_table.get("A"), Some(&0));
    assert_eq!(assembler.symbol_table.get("B"), Some(&1));
    assert_eq!(assembler.symbol_table.get("COUNT"), Some(&3));
    assert_eq!(
        &assembler.pass_two(&ast).unwrap()[12..],
        &[0x60, 0x18, 0xf3, 0x1b]
    );
}

#[test]
fn xy_branches_dollar_labels_and_conditional_decrements_keep_fixups() {
    let source="jrxn #local\njrxnn #local\njrxz #local\njrxle #local\njryn #local\njrynn #local\njrynz #local\njryle #local\ndsjeq b3,#local\ndsjne a2,#local\n#local nop\n$110 nop\njrz $110\njauc $";
    let mut assembler = Assembler::new();
    let ast = assembler
        .pass_one(&source.lines().collect::<Vec<_>>())
        .unwrap();
    assert!(omitted_instructions(&ast).is_empty());
    let binary = assembler.pass_two(&ast).unwrap();
    assert_eq!(
        binary
            .iter()
            .skip(1)
            .step_by(2)
            .take(8)
            .copied()
            .collect::<Vec<_>>(),
        [0xcc, 0xcd, 0xce, 0xc4, 0xc8, 0xc9, 0xcb, 0xc2]
    );
    let object = ObjectFile::from_assembler(Path::new("test.asm"), &assembler, &ast, &binary);
    assert_eq!(
        object
            .fixups
            .iter()
            .filter(|f| f.kind == "tms34010-relative16")
            .count(),
        2
    );
    assert!(object.fixups.iter().any(|f| f.symbol == "$110"));
    assert!(!object.fixups.iter().any(|f| f.symbol == "$"));
}

#[test]
fn branch_targets_named_like_macros_do_not_expand_and_colon_labels_emit_nothing() {
    let source =
        "DIE .macro\njauc outside\n.endm\n.ref outside\njrnz die\njruc die\n#die nop\nfont6?:";
    let mut assembler = Assembler::new();
    let ast = assembler
        .pass_one(&source.lines().collect::<Vec<_>>())
        .unwrap();
    assert!(omitted_instructions(&ast).is_empty());
    assert_eq!(
        assembler.pass_two(&ast).unwrap(),
        [0x02, 0xcb, 0, 0xc0, 0, 0, 0, 3]
    );
}
