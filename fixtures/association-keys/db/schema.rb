ActiveRecord::Schema.define do
  create_table :receipts do |t|
    t.string :slug, null: false
  end
  create_table :refunds do |t|
    t.string :slug, null: false
  end
  create_table :documents do |t|
    t.string :code, null: false
    t.integer :position, null: false
  end
  create_table :allocations do |t|
    t.string :payable_slug, null: false
    t.string :payer_kind, null: false
    t.string :document_code, null: false
    t.string :document_type
    t.string :audit_code, null: false
    t.integer :position, null: false
    t.boolean :active, null: false
  end
  create_table :accounts do |t|
    t.string :slug, null: false
  end
  create_table :case_tickets do |t|
    t.integer :account_id, null: false
  end
end
